# Chrysopoeia architecture

Chrysopoeia is a self-hosted media transcoder: point it at folders, pick a goal,
and it converts the library in the background, verifies every result, and only
then replaces the original. It is a Tdarr alternative that trades plugin stacks
for sensible defaults, automatic hardware setup and verified output.

This document is the contract between the backend crates, the web UI and the
deployment files, and it describes what the code does. Field names and JSON
shapes here are normative. The Rust source of truth for every shared type is
`crates/chrysopoeia-core`; the TypeScript mirror is `web/src/lib/types.ts`.

## Runtime shape

```
             ┌──────────────── one container, one port (8080) ────────────────┐
 browser ──► │ axum: /api/*  /api/ws  /* (static web UI from --web-dir)      │
             │   │                                                           │
             │   ├─ LibraryService: walk ─► probe (ffprobe) ─► decide ─► DB  │
             │   ├─ Watcher (notify, settle debounce) ─► LibraryService      │
             │   ├─ Dispatcher: queued jobs ─► worker::run_job (N at once)   │
             │   │     plan ─► ffmpeg (HW ► HW-enc/SW-dec ► CPU) ─► verify   │
             │   │     ─► finalize (crash-safe replace) ─► DB + events       │
             │   └─ Hardware: hwdetect::detect at startup + on demand        │
             │ SQLite (WAL) at $DATA_DIR/chrysopoeia.db                       │
             └────────────────────────────────────────────────────────────────┘
```

- Single static binary `chrysopoeia` (crate `chrysopoeia-server`) serves the
  API, the WebSocket and the exported Next.js UI. No Node at runtime.
- ffmpeg/ffprobe are external processes (jellyfin-ffmpeg in the Docker image,
  which bundles NVENC, QSV/oneVPL, VA-API (Intel iHD + AMD), AMF and Rockchip
  support for amd64 and arm64).
- The server reaches the media crates only through `toolkit::MediaToolkit`,
  which turns a panic inside a tool into an ordinary error; the integration
  tests drive the whole server with a fake toolkit.

## Crates and ownership

| Crate | Owns | Public API |
|---|---|---|
| `chrysopoeia-core` | Shared types, codec/container rules, goals, settings, events, artifact naming, tying child processes to the server's life | Everything in `src/*.rs` |
| `chrysopoeia-hwdetect` | CPU/memory/cgroup detection, GPU discovery, ffmpeg encoder listing and **test-encode verification**, job-count recommendation, setup hints | `detect`, `recommend_jobs`, `encoder_candidates`, `is_busy_failure`, `preference_problem`, `preference_hint` |
| `chrysopoeia-scanner` | Walking libraries, media extension list, ffprobe probing (async, timeout), folder watching with settle debounce | `walk_library`, `is_media_path`, `is_video_path`, `probe_file`, `parse_ffprobe_json`, `LibraryWatcher`, `ScanOptions::from_settings`, `IgnoreRules`, `validate_ignore_pattern` |
| `chrysopoeia-worker` | `plan`/`quality`: skip decision + ffmpeg args. `ffmpeg`/`run`/`validate`/`finalize`: process execution with fallback chain, verification, crash-safe replacement | `decide`, `decide_forced`, `build_plan`, `run_job`, `validate_output`, `finalize::*` (incl. `resume_replace`, `recover_artifact`) |
| `chrysopoeia-server` | Config (CLI/env), SQLite schema + migrations, REST + WS, LibraryService, Dispatcher, static UI hosting, filesystem browser | binary `chrysopoeia` |

Existing public signatures are fixed; new public items may be added.

## Configuration (server)

CLI flags, each with an env var fallback (clap `env`). Empty values (common in
container templates) mean "not set".

| Flag | Env | Default | Notes |
|---|---|---|---|
| `--port` | `PORT` | `8080` | |
| `--bind` | `BIND` | `0.0.0.0` | |
| `--data-dir` | `DATA_DIR` | `./data` (`/config` in Docker) | DB lives here |
| `--web-dir` | `WEB_DIR` | `./web/out` (`/app/web` in Docker) | Static UI; API still works if missing |
| `--ffmpeg` | `FFMPEG_PATH` | `ffmpeg` | |
| `--ffprobe` | `FFPROBE_PATH` | `ffprobe` | |
| `--browse-root` | `BROWSE_ROOTS` (comma-sep) | `/` | Roots the folder picker may show |
| `--temp-dir` | `TEMP_DIR` | unset | Work folder used while `settings.temp_dir` is unset (`/temp` in Docker when mounted). Unset: encodes are written next to the file (folder mode: into the output folder) |
| `--max-jobs` | `MAX_JOBS` | unset | 1–32 or `auto`. Stands in for the automatic job count whenever `settings.max_jobs` ("Files at once" in the UI) is null; a number saved in Settings wins, and the activity feed says so at start ("MAX_JOBS=3 is not used because Files at once is set to 2 in Settings. Choose Automatic there to use MAX_JOBS.") |
| `--hw` | `HW_ACCEL` | `auto` | auto, cpu, nvenc/nvidia, qsv/intel, vaapi, amf, videotoolbox, rkmpp, v4l2m2m. Sets the `hardware` setting on the first run and again whenever the value changes; otherwise the Settings choice is kept |
| `--library` | `LIBRARIES` (comma-sep) | none | Libraries created on the first run |
| `--allowed-host` | `ALLOWED_HOSTS` (comma-sep) | none | Extra host names (e.g. a reverse proxy's domain; `.example.com` covers a domain; `*` = any) |
| `--log-level` | `LOG_LEVEL` | `info` | `RUST_LOG` wins when set |
| `--dev-cors` | `DEV_CORS` | false | Permissive CORS and no origin check, for `next dev` on another port |

Not configurable: a file must go 20 s without changes (size and mtime) before
it is picked up or converted (the settle time); the database busy timeout is
30 s. Active hours use the local time zone (`TZ`).

Start order: exclusive lock on `$DATA_DIR/chrysopoeia.lock` (a second
Chrysopoeia on the same data folder exits with "Another Chrysopoeia is already
using the data folder …" and changes nothing; filesystems that can't lock
only log a warning) → bind the port → open the database → recovery.

## Database (SQLite, WAL)

Migrations are versioned with `PRAGMA user_version` and applied step by step,
keeping data. A pre-v1 database (the prototype's tables) is dropped and
recreated; it only ever held re-scannable data. A database from a newer
version is refused with a plain message.

```
settings(key TEXT PK, value TEXT)   -- 'settings' → Settings JSON; flags 'queue_paused',
                                    -- 'clean_shutdown', 'hw_accel_applied'
libraries(id TEXT PK, name, path UNIQUE, enabled INT, profile TEXT JSON,
          last_scan_at TEXT NULL, created_at TEXT, settling INT DEFAULT 0)
files(id TEXT PK, library_id FK→libraries ON DELETE CASCADE, path UNIQUE, relative_path,
      file_name, size_bytes INT, modified_at TEXT, status TEXT, probe TEXT JSON NULL,
      container, video_codec, audio_codec, resolution, hdr, duration_secs REAL, bit_rate INT,
      original_size_bytes INT NULL, saved_bytes INT NULL, skip_reason, error,
      job_id TEXT NULL, scanned_at, updated_at)
      INDEX(library_id, status), INDEX(status)
jobs(id TEXT PK, file_id FK→files ON DELETE CASCADE, library_id, file_name, file_path,
     state TEXT, stage TEXT, priority INT, progress REAL, fps REAL, speed REAL, eta_secs INT,
     encoder, hw_api, attempt INT, input_size INT, output_size INT, error, skip_reason,
     validation TEXT JSON, command, log_tail, notes TEXT JSON NULL,
     created_at, started_at, finished_at, final_path TEXT NULL, force INT DEFAULT 0)
     INDEX(state, priority DESC, created_at), INDEX(file_id, created_at), INDEX(created_at),
     partial INDEX(finished_at) of finished jobs
activity(id INTEGER PK AUTOINCREMENT, at, level, message, file_id, job_id, library_id)
savings(date TEXT 'YYYY-MM-DD', library_id FK→libraries ON DELETE CASCADE, saved_bytes INT,
        files INT, PK(date, library_id))
```

Versions: 1 = base schema; 2 = job-list indexes and `finished_at` on every
finished job; 3 = `jobs.notes`; 4 = `jobs.final_path` (where a started job
puts its result, written when it starts); 5 = `jobs.force` ("Convert
anyway"), `libraries.settling` (files the last scan left for later because
they were still being copied) and `savings` keyed by day and library (with
one library the old daily rows become its own; with several the history is
rebuilt from the finished jobs on record, which cover the 30 days shown).

Rules:
- Timestamps are RFC 3339 UTC strings with milliseconds. UUIDs are hyphenated
  lowercase strings. Write transactions use `BEGIN IMMEDIATE`.
- `files.size_bytes`/`modified_at` always describe the file currently on disk.
  After a replace they describe the new file, so rescans see it unchanged.
- Activity keeps the newest 5 000 rows. Finished jobs are trimmed to the
  newest 5 000 and 90 days, except the job each file points at.
- Savings rows belong to a library and go with it, so the savings chart
  (`Overview.savings_history`) and the total above it
  (`Overview.totals.saved_bytes`, the files listed now) describe the same
  libraries.

## File and job lifecycle

```
scan/watch ─► probe ─► decide(profile)
                        ├─ Skip{reason}          → file.status = skipped
                        └─ Transcode             → pending, or queued + job(queued) if auto_queue
dispatcher claims job ─► running(preparing ► transcoding ► verifying ► finalizing)
   ├─ Done     → file: done, size=new size, original_size, saved_bytes;
   │             savings[today, library] += saved
   ├─ Skipped  → file: skipped + reason (e.g. "Only 3% smaller — kept the original");
   │             a file converted before (original_size set) stays done with its
   │             savings, and only the job row records the skip
   ├─ Failed   → file: failed + error; original untouched
   └─ Cancelled→ file: pending (queued again after "Stop now" or shutdown; skipped after "Skip")
```

- Scans: walk (blocking thread) → compare with the DB by path, size and mtime
  → probe new/changed files (4 at a time, 60 s each) → decide with the
  profile read right before each batch of 50 is written. Files still settling
  are looked at again later; their number is stored on the library at the
  end of each scan (`LibraryStats.settling`) and lowered as they settle
  (a later scan of the library takes over the count). A library whose last
  scan left such files is scanned again at start. Removed files are deleted
  from the DB (their jobs cascade), except below folders the walk couldn't
  read or left alone, and never when a library that had files walks empty
  (an unmounted share). A file that is still on disk is never removed.
- Scans at start (after recovery and once the watcher is armed): every
  enabled library when `watch_folders` is on or `rescan_interval_hours` > 0
  (catching what changed while the server was down), otherwise only
  libraries never scanned. Periodic rescans follow `last_scan_at`.
- Walk results carry `errors` (unreadable folders: a warning in the feed) and
  `notes` (DVD/Blu-ray disc folders, skipped links, names that aren't UTF-8,
  unusable ignore patterns: an informational "Left alone in …" entry). Each is
  reported once per server run. Notes on the library folder itself (unusable
  ignore patterns) don't keep removed files listed.
- Filters (scan and watcher alike): ignore patterns (scanner glob rules,
  validated on save) and `min_file_size_mb` in decimal megabytes
  (1 MB = 1 000 000 bytes). The minimum size only decides which files are
  added: files already listed stay while they are on disk (a conversion often
  ends up below the minimum).
- Files `queued`/`processing` are not touched by scans or watch events. Files
  `done` whose size/mtime still match are left alone. A file in `failed` is
  not re-queued automatically; the user retries.
- Startup recovery: first, jobs left `running` whose new file was already in
  place are finished and recorded as `done` (note: "Chrysopoeia stopped just
  as the new file was being put in place. The new file was already complete,
  so it was kept"): in replace mode when `worker::finalize::resume_replace`
  finds the job's backup of the original next to a file under
  `jobs.final_path`; in folder mode when the job was `finalizing`, its output
  file exists and no other finished job claims that path. Then jobs `running`
  → `queued` (attempt reset), files `processing` → `queued`. Then every
  temp/backup artifact is passed to `worker::finalize::recover_artifact`: in
  the temp folders always, and in the library folders and the output folder
  after an unclean shutdown.

## Folder watching

- One kernel watcher per library root, armed from a blocking thread; roots are
  watched with `watch_with_options(root, ScanOptions::from_settings)` and
  watched again when `ignore_patterns` or `min_file_size_mb` change. No lock
  is held across a watcher call.
- A new or changed file is reported once no event arrived for it for the
  settle time and its size/mtime held still that long; removals (files and
  folders) are reported at once, except renames to Chrysopoeia's backup names
  and the loss of a whole root.
- Renames and moves (a watcher removal plus a new file, or a scan that finds
  a row's file gone and a new one): a new file with the same size and
  modification time as a row removed from the same library within the last
  hour takes over that row's status, probe, reasons and savings (not its job
  history), so it is neither probed nor converted again. Queued files and
  files whose last job found them missing are decided afresh.

## Dispatcher

- Loop woken by `Notify` (new job, settings change, job finished) plus a 5 s tick.
- Effective `max_jobs` = `settings.max_jobs`, else `MAX_JOBS`, else
  `hardware.recommended_jobs.total` (automatic), clamped to 1–32. Changes
  apply immediately; running jobs are never killed to shrink.
- While the limit is automatic, jobs whose first-choice encoder is software
  are capped at `recommended_jobs.cpu_jobs` at once (the automatic limit may
  follow the GPU count); other libraries' GPU jobs still fill the free slots.
- Does not start jobs before hardware detection and crash recovery finished,
  when paused, or outside `active_hours`.
- Claim: the `queued` job with the highest priority, then oldest, of an
  enabled library; marked running in the same transaction.
- Jobs of a library whose folder is offline (missing, unreadable or empty)
  wait, and the folder is checked again every 15 s; a job whose file changed
  moments ago waits for it to settle.
- Each running job has a `CancellationToken`. Cancel → file `pending`;
  "Stop now" and shutdown → job and file back to `queued`; deleting a library
  cancels its jobs (media untouched). ffmpeg is killed at once, so
  `POST /jobs/{id}/cancel` answers within a moment with the job's final
  state; the job leaves the running set (and `queue.state`) only after its
  result is recorded. A job being put in place (finalizing) is not
  interrupted; the call then waits up to 8 s and returns the job as it is.
- Progress: every update is broadcast as `job.progress`; the DB is written on
  stage changes and at most every 2 s per job.
- A result is never lost: recording it retries while the database is busy.
- Encoder candidates come from `hwdetect::encoder_candidates(hw, profile.video_codec,
  settings.hardware, settings.cpu_fallback)`, else the codec's software encoder.
- When `settings.hardware` names an API that can't make the library's codec
  here (`hwdetect::preference_problem`), the job gets a note ("You chose
  NVIDIA NVENC, but no working NVIDIA encoder was found here, so this file
  was converted on the CPU"); with `cpu_fallback` off it fails instead
  ("…, and converting on the CPU instead is turned off, so this file wasn't
  converted. Choose Automatic under Hardware in Settings, or allow CPU
  fallback.") and nothing is encoded.
- `jobs.force` is passed to the worker as `JobSpec.force`.
- Stored probes of PQ video without HDR10 mastering data are refreshed at job
  start (older versions didn't read it).
- A failure is logged once, at WARN, through its activity entry.
- Logging: the start line names the version and, when set, the build
  (`CHRYSOPOEIA_VERSION`); each hardware detection logs one INFO line
  ("hardware detection finished", with hwdetect's own summary at DEBUG);
  each finished job logs one plain INFO line, its activity entry ("Converted
  …", "Skipped …", "Cancelled …"; failures at WARN). Job starts, attempts
  and fallbacks are logged at DEBUG.
- Watch events for a file are ignored while it is `queued`/`processing`;
  after its job ends the file is looked at again (as a watch event would).
  A file moved or deleted during its job (gone while its library folder is
  there) is taken off the list with watching on ("… was moved or deleted
  while it was being converted, so it was taken off the list."), or marked
  missing (`The file is no longer at …`) with watching off.

## Worker behaviour (normative)

**decide** skips when: no real video stream ("Audio-only file — nothing to
convert"); unknown picture size; known duration under 1 s; `skip_efficient`
and source codec efficiency rank ≥ target rank ("Already AV1…"); not
`skip_efficient` and the file already is the target codec in the target
container family, 4:2:0, ≤ 10-bit (8-bit for H.264), with audio the container
holds ("Already H.264 in an MP4 or MOV file"). The efficiency rules don't apply
when the picture exceeds `max_height`. Always skipped: Dolby Vision without a
standard base layer — the scanner sets `StreamInfo.dolby_vision_without_base_layer`
(profile 5, compatibility id 0) and clears its colour tags: "Dolby Vision
profile 5 can't be converted without losing its colours — left unchanged";
any other Dolby Vision stream without a colour transfer: "This Dolby Vision
video doesn't say which colours its picture uses, so converting it could ruin
them — left unchanged"; HDR to H.264 ("HDR video would lose its colours as
H.264 — left unchanged"); files whose audio can't be read. `run_job` repeats
`decide` (a user may queue a skipped file). `decide_forced` ("Convert
anyway") is `decide` without the efficiency and same-format rules; a job
with `JobSpec.force` uses it when `decide` says skip.

**build_plan**:
- Explicit `-map 0:<index>` per kept stream. Primary video only (no cover art,
  no data/timecode streams). Audio filtered by `audio_languages` (never drop all
  audio). Subtitles per `Container::subtitle_action` and `subtitle_languages`;
  attachments only for MKV.
- `-map_metadata 0 -map_chapters 0`, `-max_muxing_queue_size 9999`,
  `-analyzeduration 100M -probesize 100M` on input, `-f <muxer>`.
- Audio per output stream: copy when the profile says copy (or source already in
  target codec) and the container can hold it; else encode with
  `AudioCodec::default_bitrate_kbps(channels)`, downmixing past
  `max_channels()`. Opus with >2 channels: `-mapping_family 1` and a channel
  layout normalization filter (libopus rejects `5.1(side)`).
- Video: keep 10-bit for sources deeper than 8 bits when the target and API
  support it (`yuv420p10le` / `p010le`), else `yuv420p` / `nv12`. Pass colour
  primaries / transfer / matrix through (never `unknown`, never `gbr`). HDR10
  keeps its mastering display and light levels: libx265 gets
  `master-display`/`max-cll` (plus `hdr-opt=1:repeat-headers=1`), libsvtav1
  gets `-svtav1-params mastering-display=…:content-light=…`. Deinterlace
  (`bwdif`) interlaced sources (forces CPU decode). Downscale when `max_height`
  is exceeded. Odd sizes are scaled to even and noted.
- Quality from `quality.rs`: `QualityLevel` → encoder scale (CRF for x264/x265/
  SVT-AV1/libaom/libvpx, `-cq` NVENC, `-global_quality` QSV, `-qp`/`-rc_mode`
  VA-API, `-q:v` VideoToolbox, `-qp_i/-qp_p` AMF). `quality_override` wins when
  it fits the encoder's scale (else a note says it was ignored).
  `SpeedPreset` → encoder preset.
- HW init per API, e.g. NVENC `-hwaccel cuda -hwaccel_output_format cuda` when
  `hw_decode`; VA-API `-init_hw_device vaapi=va:<node>` (+`-hwaccel vaapi
  -hwaccel_output_format vaapi` when `hw_decode`, else `format=nv12,hwupload`);
  QSV via `-init_hw_device qsv=qs:...`; VideoToolbox/AMF accept system frames.
- MP4: `-movflags +faststart`, HEVC gets `-tag:v hvc1`.
- Compromises (dropped subtitles, 8-bit reduction, resizes, fallbacks) are
  returned as plain-language `notes`.

**run_job**:
1. Preparing: the input exists, `decide` agrees (`decide_forced` with
   `force`), the destination is free and
   its folder writable, and the temp folder has room (1.1× the original,
   counting space promised to other running jobs; a job waits for room other
   jobs hold). The input's size+mtime (`FileIdentity`) is recorded.
2. Transcoding: for each candidate, an attempt with `hw_decode` as given; a
   failed hardware attempt is retried with CPU decoding, then the next
   candidate. Stop at the first success. Cancellation kills ffmpeg at once
   (SIGKILL: the temp output is discarded anyway, and x265/SVT-AV1 ignore
   SIGTERM for seconds while they flush) and removes the temp file. A
   process silent for 10 min counts as hung and is killed the same way.
3. A cut-off original: when ffmpeg reported damaged input or verification
   found the result too short, and the encode ended clearly before the length
   the container claims (> max(2 s, 5 %)), the job fails with "The original
   file appears damaged or incomplete (it stops after 0.1 s). It was left
   unchanged." (With verification off, ending before half the length is
   enough.) Only an attempt that decoded on the CPU (or the last attempt)
   concludes this; a GPU-decoding attempt that stops early moves on to the
   next attempt like any other hardware failure.
4. **Size rule**: if `profile.min_savings_pct = Some(p)` and the output is not
   at least `p`% smaller, it is discarded: `Skipped` ("Only 4% smaller — kept
   the original", "The new file was 7% larger — kept the original"). Not
   applied with `force`.
5. The original's identity is compared again; a changed original (e.g. a
   Sonarr/Radarr upgrade) gives `Skipped` ("The original changed while it was
   being converted, so it was left alone").
6. Verifying (below). A hardware result that fails moves on to the next
   attempt; any other failure fails the job.
7. Finalizing (below).

stderr noise that means nothing (libnuma's `set_mempolicy: Operation not
permitted` from libx265 under Docker's seccomp profile) is kept out of error
reasons and log tails. Every ffmpeg/ffprobe child is started with
`core::process::end_with_parent` (Linux `PR_SET_PDEATHSIG` = SIGKILL), so no
encode outlives a killed server.

**validate_output** by level (checks stop at the first failure):
- `quick`: ffprobe opens the output; stream counts match the plan; video codec
  is the target; duration (picture and sound streams) within max(1 s, 0.5 %).
- `standard`: quick + full decode of every picture and sound track (error/fatal
  lines, "corrupt decoded frame" and concealment count as damage; damage the
  original already has only warns) + visual comparison at 4 segments of 2 s.
- `thorough`: 10 segments + a whole-file frame comparison at ~320×180 +
  black/frozen frame totals (the output may not add more than 2 s of either).
- Visual comparison: both files decoded, the source scaled to the output size,
  both reduced to ~640×360 (area), aligned on the shared timeline within ±3
  frames. Fail if any frame SSIM < 0.60 or any segment mean < 0.85; warn if
  the overall mean < 0.93.
- Every check has a stable id, a plain label (what passing means, e.g. "Same
  length as the original") and one sentence of detail. A failed result's
  `job.error` (and its activity entry) is phrased as the failure, per check
  id, never with the pass-form label: `duration` and `probe` use the detail
  ("The new file is shorter than the original (0.1 s instead of 8.0 s)");
  `streams`, `decode` and `visual` lead with the problem ("The new file
  doesn't look like the original. A frame near 0:10 looks very different
  …").

**finalize**: the verified temp file is first staged under a hidden name in the
destination folder (a rename, or across filesystems a copy that is flushed to
disk), so the original stays until the new file is completely there. The
original's identity is checked again right before it is touched. Replace mode
(same path or new extension): original → hidden backup
(`paths::backup_file_name`), staged → final, backup deleted; the backup is
checked to be the file the job read, a new name must still be free when the
new file moves in, and any failure puts the backup back. (A backup name longer
than 255 bytes: the new file is renamed in first, then a renamed original is
deleted.) Folder mode: create folders, never overwrite, never touch the
original. The new file gets the original's permission bits (and owner where
allowed) and, with `keep_file_dates`, its times. After a crash,
`resume_replace(input, final_path, job_id)` reports `Placed` (and deletes the
backup) when the job's backup exists and the new file is in place — the
original's name taken again (same path) or free with the new name present
(new extension) — else `NotPlaced`. `recover_artifact`: temp and staged files
are deleted; a backup is renamed back when the original is missing, deleted
otherwise.

## REST API (`/api`)

All responses are JSON. Errors: HTTP 4xx/5xx with
`{"error": "<plain sentence>", "code": "<snake_case>"}`, plus
`"field": "<name>"` when one setting or request field is at fault (a settings
key such as `temp_dir`, a nested one such as `default_profile.quality`, or
`path`, `name`, `profile.max_height`, `profile.quality`). A value of the wrong
kind is explained in plain words ("The value for "default_profile.quality"
isn't valid. Choose one of: …"). 500s carry a generic sentence; details go to
the log. List endpoints return `{"items": [...], "total": <n>}`. Bodies are
limited to 1 MB (413 `body_too_large`); write requests with a body need
`Content-Type: application/json` (415 `unsupported_media_type`).

| Method & path | Body / query | Returns |
|---|---|---|
| `GET /health` | | `{"ok":true,"version":"0.2.0"}` |
| `GET /system` | | `SystemInfo {version, build, default_temp_dir, browse_roots, data_dir, in_container}` (`build` from `CHRYSOPOEIA_VERSION` when it differs from `version`, else null) |
| `GET /overview` | | `Overview` |
| `GET /libraries` | | `Library[]` (`path_error` set when the folder is missing, unreadable or offline) |
| `POST /libraries` | `{"path", "name"?, "profile"?, "goal"?}` | `Library` (201). 400 `path_required`/`path_not_absolute`/`path_not_found`/`not_a_directory`/`not_readable`/`path_not_supported`/`contains_output_folder`/`invalid_name`/`invalid_profile`, 409 `library_exists`/`library_overlaps`. Starts a scan. |
| `GET /libraries/{id}` | | `Library` |
| `PATCH /libraries/{id}` | `{"name"?, "enabled"?, "profile"?}` (profile is normalized; response includes it) | `Library`. Profile changes re-decide `pending`/`skipped` files (not done/failed; files skipped by the user stay skipped). |
| `DELETE /libraries/{id}` | | 204. Removes DB rows only (files, jobs and its share of the savings history), never media. Cancels its running jobs. |
| `POST /libraries/{id}/scan` | | 202 `{"started":true}` (409 `scan_running`, 409 `library_disabled`) |
| `POST /scan` | | 202, scans all enabled libraries |
| `GET /files` | `status`, `library`, `q` (substring of name/path), `sort` (`name`,`size`,`updated`,`status`; prefix `-` for desc), `limit` (≤500, default 100), `offset` | `{"items": MediaFile[], "total"}` (no `probe`) |
| `GET /files/{id}` | | `{"file": MediaFile (with probe), "jobs": Job[] (newest first, ≤10)}` |
| `POST /files/{id}/queue` | `{"priority"?: int, "force"?: bool}` | `Job` (`force` echoed). Works for pending/failed/skipped/done (re-encode). `force` = "Convert anyway" (see `decide_forced`, no size rule; verified as usual). A done file whose new job ends skipped stays done. 409 `already_queued`. |
| `POST /files/{id}/skip` | | `MediaFile` status skipped, reason "Skipped by you"; cancels its job |
| `POST /files/bulk` | `{"action":"queue"\|"skip"\|"retry_failed", "ids"?: [], "library"?, "status"?}` | `{"affected": n, "left_out": m}`. `queue` with `ids` only queues failed files and files the library's goal would convert (`decide`); the rest are counted in `left_out` (0 for other selections). |
| `GET /jobs` | `state` (`active` = running+queued, `running`, `queued`, `history` = finished, `all`), `limit`, `offset` | `{"items": Job[], "total"}`; active sorted running-first then queue order; history newest first |
| `GET /jobs/{id}` | | `Job` (includes `notes: string[]`, `validation`, `command`, `log_tail`) |
| `POST /jobs/{id}/cancel` | | `Job` (409 `job_finished`) |
| `POST /jobs/{id}/priority` | `{"priority": int}` or `{"move":"top"}` | `Job` |
| `POST /jobs/clear` | `{"state":"history"}` | `{"affected": n}` deletes finished job rows (files keep status) |
| `GET /queue` | | `QueueState` |
| `POST /queue/pause` / `POST /queue/resume` | | `QueueState` |
| `POST /queue/stop` | | `QueueState` (cancel running, re-queue them, pause) |
| `GET /settings` | | `Settings` |
| `PATCH /settings` | partial `Settings` JSON (merged at top level; `default_profile` replaced whole) | `Settings`. 400 `invalid_settings`/`unknown_setting` with `field` (e.g. folder mode without folder, unwritable temp dir, an added ignore pattern that is invalid; patterns already saved don't block other changes) |
| `GET /hardware` | | `HardwareInfo` (`detecting: true` placeholder until the first detection ends) |
| `POST /hardware/detect` | | `HardwareInfo` (re-runs detection, ~seconds) |
| `GET /presets` | | `{"goals": [{"goal","title","summary","profile"}], "video_codecs": [{"codec","label","royalty_free","hw_accelerated", "encoders": [verified names]}], "audio_codecs": [{"codec","label"}], "containers": [{"container","label","video": [...], "audio": [...]}]}` — only codecs with a verified encoder (a listed CPU encoder when detection failed) and audio codecs whose encoder ffmpeg has (plus `copy`); everything while detection runs |
| `GET /fs/browse` | `path` (default: first browse root) | `{"path","parent": string\|null,"roots": [string],"entries":[{"name","path","is_dir":true,"media_count"?: n,"media_count_capped"?: bool}]}` directories only, sorted, hidden dirs and links out of the roots excluded. `media_count`: video files (not audio-only ones) in the folder and up to 4 levels below, hidden entries skipped, links to folders not followed; counting stops after 2 000 entries or ~150 ms per folder (`media_count_capped: true`, "at least n"), and after ~2 s per listing, or 300 folders, later folders get no count. 400 `path_not_absolute`/`not_a_directory`/`not_readable`, 403 `outside_roots`, 404 `path_not_found`/`no_browse_roots` |
| `GET /activity` | `limit` (≤500, default 100), `before` (id) | `{"items": ActivityEntry[]}` newest first |
| `GET /ws` | WebSocket | `Event` JSON messages (see `core::event`) |

Other codes: 404 `not_found` (unknown path), `library_not_found`,
`file_not_found`, `job_not_found`; 405 `method_not_allowed`; 400
`invalid_json`, `invalid_request` (with `field` when serde names one),
`invalid_query`, `invalid_path_param`, `invalid_status`, `invalid_sort`,
`invalid_state`, `invalid_library`; 403 `host_not_allowed`,
`forbidden_origin`; 409 `job_finished`; 413 `body_too_large`; 415
`unsupported_media_type`. An ignore pattern that can't be used is refused
with a plain reason naming it (`The ignore pattern "Movies/[abc" has a [
without a closing ].`), never the glob library's wording.

WebSocket: on connect the server sends `queue.state` and `stats.updated`
immediately. Server pings every 30 s. A lagging client just misses events;
clients refetch on reconnect.

**Request guard** (no accounts; trusted LAN): the `Host` (or
`X-Forwarded-Host`) must be an IP literal, `localhost`, a single-label name,
a name under `.local`, `.lan`, `.home`, `.home.arpa`, `.internal`,
`.localdomain`, `.localhost`, `.ts.net`, `.fritz.box`, or listed in
`ALLOWED_HOSTS`; else 403 `host_not_allowed` (DNS rebinding). Requests that
change something and WebSocket upgrades carrying an `Origin`/`Referer` must
come from the same host; else 403 `forbidden_origin`. When `Host` (or
`X-Forwarded-Host`) has no port, as reverse proxies such as Nginx Proxy
Manager send it, only the host names are compared (`Origin:
https://name:8443` matches `Host: name`); the host allowlist still keeps
other websites out. Both refusals end with "If you use a reverse proxy,
make it pass the original Host header." Requests without either (curl,
scripts) pass. No CORS headers are sent (except with `--dev-cors`).

## Hardware detection (normative)

- CPU: model from `/proc/cpuinfo` (fallback: "Unknown CPU"), logical cores via
  `available_parallelism`, cgroup v2 `cpu.max` / v1 `cpu.cfs_quota_us`.
- Memory: `/proc/meminfo` + cgroup `memory.max` / `memory.limit_in_bytes`.
- GPUs: `/sys/class/drm/renderD*/device/{vendor,driver}` (0x8086 Intel,
  0x1002 AMD, 0x10de NVIDIA); names from `lspci -mm` if present, else
  `/sys/.../device/product_name`/`label`, else vendor + node. NVIDIA via
  `nvidia-smi --query-gpu=name,driver_version --format=csv,noheader` and
  `/proc/driver/nvidia/gpus`. On macOS: VideoToolbox assumed present.
- Encoders: every registry encoder is reported; `available` = listed by
  `ffmpeg -hide_banner -encoders`. Software encoders listed are `verified`
  without a test. Each listed hardware encoder gets a 1-second 256×256 test
  encode with the same init flags `build_plan` uses (15 s each, 30 s in all,
  NVENC one at a time, a hung GPU is not tested again). Unverified encoders
  say why in `error` ("<sentence> Details: <ffmpeg tail>").
- Busy NVIDIA GPU (every encoding session taken, e.g. by Chrysopoeia's own
  running jobs or by Plex): tested again once after 3 s. If it stays busy, an
  encoder the previous detection verified keeps that result (and the busy
  hint goes); otherwise the busy hint stays and detection runs again by itself
  after 3 minutes (up to 10 times in a row).
- Hints (plain language, with copy-paste fixes): ffmpeg missing; NVIDIA device
  visible but NVENC fails → `--runtime=nvidia` + `NVIDIA_VISIBLE_DEVICES=all`
  + `NVIDIA_DRIVER_CAPABILITIES=all` (Unraid: install the Nvidia-Driver plugin);
  `/dev/dri` missing → `--device=/dev/dri`; render node present but permission
  denied → group/PGID advice; Intel GPU without QSV → `intel-media-driver`;
  no GPU at all → CPU encoding is fine, here is the expected speed.
- `settings.hardware` naming an API with no verified encoder: the server
  adds `hwdetect::preference_hint` ("The hardware you chose isn't working":
  "You chose NVIDIA NVENC, but no working NVIDIA encoder was found here, so
  files are converted on the CPU. …"; an error when `cpu_fallback` is off),
  after every detection and whenever the preference or CPU fallback
  changes.
- `recommend_jobs`: CPU jobs = clamp(floor(effective_cores / 4), 1, 8), further
  capped by memory (1.5 GB per job) — effective cores honor cgroup limits.
  GPU jobs: NVIDIA 3 per GPU (consumer NVENC session limits), Intel/AMD 2 per
  GPU, Apple 2; capped by effective cores and memory. `total` = GPU jobs when
  a verified hardware encoder exists (for the pinned API, when one is pinned)
  and the preference ≠ cpu, else CPU jobs.
- `HardwareInfo.detecting` is true only for the placeholder served while the
  first detection runs; the UI shows "Checking your hardware…" from it.

## Web UI

Next.js (App Router) exported statically (`output: "export"`) to `web/out` and
served by the Rust binary. All data comes from `/api` on the same origin
(`NEXT_PUBLIC_API_URL` overrides for `next dev`). No mock data in the bundle.

Screens (sidebar navigation; state in the URL hash so reloads keep the view):
1. **Setup** (first run, when `settings.onboarded` is false and no libraries):
   welcome → pick a folder with the server-side folder browser → choose a goal
   (cards: Save space / Balanced / Plays everywhere / Archive, each with a one-line
   trade-off and the detected hardware's speed hint) → "Start" (creates the
   library, scan begins, `onboarded = true`).
2. **Overview**: space saved (big number) + projected savings, library progress,
   now-processing cards with live progress/fps/ETA/encoder badge, recent results
   with a "Verified" badge, setup hints from hardware detection, codec and
   resolution breakdown (files without a video stream count as "No video";
   "Unknown" is a video whose picture size couldn't be read).
3. **Queue**: Running / Up next / History (done, skipped, failed) with cancel,
   move to top, retry, and a detail sheet (streams before→after, notes,
   verification report with SSIM, ffmpeg command, log tail).
4. **Library** (per library): file table with search, status filter, sort,
   pagination, bulk actions; library settings (goal, quality, advanced).
5. **Settings**: Processing (Files at once: Automatic (n) or a number; active
   hours; auto-queue; watch folders), Output (replace vs folder; work folder,
   naming `SystemInfo.default_temp_dir` as the automatic one; keep dates),
   Verification level, Hardware (detected devices, verified encoders, hints,
   check again, preference), Advanced (ignore patterns, min size in MB,
   default profile). Field errors are shown next to the field named by `field`.

Plain language first: say "Smaller files" not "CRF 32"; show codecs as
secondary detail; every destructive action is reversible or confirmed.

## Deployment

- `Dockerfile`: multi-stage (Rust build, web export, runtime on
  `debian:bookworm-slim` + `jellyfin-ffmpeg7`), multi-arch amd64/arm64.
  Entrypoint handles `PUID`/`PGID`/`UMASK` (Unraid defaults 99/100), adds the
  user to the groups owning `/dev/dri/*`, then drops privileges.
- Volumes: `/config` (DB), `/media` (libraries), `/temp` (optional work
  folder, put it on an SSD/cache pool). One port: 8080.
- GPU: NVIDIA via `--runtime=nvidia` (Unraid Nvidia-Driver plugin) or compose
  `deploy.resources.reservations.devices`; Intel/AMD via `--device=/dev/dri`.
- `unraid/chrysopoeia.xml`: Community Applications template with those fields.
- `docker-compose.yml` (CPU), `docker-compose.nvidia.yml`,
  `docker-compose.intel-amd.yml` overlays.

## Contract additions, round 3 (normative)

- `QueueState.max_jobs_source`: `auto` | `env` | `settings` — where the
  effective job limit comes from. The UI shows e.g. "Automatic (3, from
  MAX_JOBS)" for `env`.
- `LibraryStats.settling`: files the last scan deferred because they are still
  being copied; the UI shows "Waiting for N files to finish copying" instead
  of parsing activity text.
- `SystemInfo.build`: the image build label from `CHRYSOPOEIA_VERSION`, when
  it differs from `version`; shown in Settings for bug reports.
- `POST /api/files/{id}/queue` accepts `{"priority"?: int, "force"?: bool}`.
  `force: true` runs that one job without the skip rules (`skip_efficient`,
  same-format) and without `min_savings_pct` ("Convert anyway"). Verification
  still applies. A forced job that finishes keeps the file `done`.
- A re-queued `done` file whose job ends `skipped` stays `done` (its savings
  are kept); the job row records the skip.
- `POST /api/files/bulk` with `action: "queue"` and explicit `ids` only
  creates jobs for files the profile would convert (or failed files) and
  returns `{"affected": n, "left_out": m}`.
- `Job.force` (bool) echoes the queue request's `force`.
- `GET /api/fs/browse` entries: `media_count` counts video files below the
  folder (see the endpoint), with `media_count_capped`.
- `Overview.resolutions` has a "No video" bucket; `savings_history` and
  `totals.saved_bytes` cover the same libraries.
- Workspace MSRV is Rust 1.88.
