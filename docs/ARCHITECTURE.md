# Chrysopoeia architecture

Chrysopoeia is a self-hosted media transcoder: point it at folders, pick a goal,
and it converts the library in the background, verifies every result, and only
then replaces the original. It is a Tdarr alternative that trades plugin stacks
for sensible defaults, automatic hardware setup and verified output.

This document is the contract between the backend crates, the web UI and the
deployment files. Field names and JSON shapes here are normative. The Rust
source of truth for every shared type is `crates/chrysopoeia-core`.

## Runtime shape

```
             ┌──────────────── one container, one port (8080) ────────────────┐
 browser ──► │ axum: /api/*  /api/ws  /* (static web UI from --web-dir)      │
             │   │                                                           │
             │   ├─ LibraryService: walk ─► probe (ffprobe) ─► decide ─► DB  │
             │   ├─ Watcher (notify, debounced) ─► LibraryService            │
             │   ├─ Dispatcher: queued jobs ─► worker::run_job (N at once)   │
             │   │     plan ─► ffmpeg (HW ► HW-enc/SW-dec ► CPU) ─► verify   │
             │   │     ─► finalize (atomic replace) ─► DB + events           │
             │   └─ Hardware: hwdetect::detect at startup + on demand        │
             │ SQLite (WAL) at $DATA_DIR/chrysopoeia.db                       │
             └────────────────────────────────────────────────────────────────┘
```

- Single static binary `chrysopoeia` (crate `chrysopoeia-server`) serves the
  API, the WebSocket and the exported Next.js UI. No Node at runtime.
- ffmpeg/ffprobe are external processes (jellyfin-ffmpeg in the Docker image,
  which bundles NVENC, QSV/oneVPL, VA-API (Intel iHD + AMD), AMF and Rockchip
  support for amd64 and arm64).

## Crates and ownership

| Crate | Owns | Public API |
|---|---|---|
| `chrysopoeia-core` | Shared types, codec/container rules, goals, settings, events, artifact naming | Everything in `src/*.rs` |
| `chrysopoeia-hwdetect` | CPU/memory/cgroup detection, GPU discovery, ffmpeg encoder listing and **test-encode verification**, job-count recommendation, setup hints | `detect`, `recommend_jobs`, `encoder_candidates` |
| `chrysopoeia-scanner` | Walking libraries, media extension list, ffprobe probing (async, timeout), folder watching with settle debounce | `walk_library`, `is_media_path`, `probe_file`, `parse_ffprobe_json`, `LibraryWatcher` |
| `chrysopoeia-worker` | `plan`/`quality`: skip decision + ffmpeg args. `ffmpeg`/`run`/`validate`/`finalize`: process execution with fallback chain, verification, crash-safe replacement | `decide`, `build_plan`, `run_job`, `validate_output`, `finalize::*` |
| `chrysopoeia-server` | Config (CLI/env), SQLite schema + migrations, REST + WS, LibraryService, Dispatcher, static UI hosting, filesystem browser | binary `chrysopoeia` |

Stub signatures in each crate's `lib.rs` are fixed. Implementations may add
private helpers and new public items, but must not change existing signatures.

## Configuration (server)

CLI flags, each with an env var fallback (clap `env`):

| Flag | Env | Default | Notes |
|---|---|---|---|
| `--port` | `PORT` | `8080` | |
| `--bind` | `BIND` | `0.0.0.0` | |
| `--data-dir` | `DATA_DIR` | `./data` (`/config` in Docker) | DB lives here |
| `--web-dir` | `WEB_DIR` | `./web/out` (`/app/web` in Docker) | Static UI; API still works if missing |
| `--ffmpeg` | `FFMPEG_PATH` | `ffmpeg` | |
| `--ffprobe` | `FFPROBE_PATH` | `ffprobe` | |
| `--browse-root` | `BROWSE_ROOTS` (comma-sep) | `/` | Roots the folder picker may show |
| `--temp-dir` | `TEMP_DIR` | unset | Default scratch dir if the setting is unset. Docker image sets `/temp` when that mount exists |
| `--max-jobs` | `MAX_JOBS` | unset | Overrides the automatic job count at boot (settings still win once saved) |
| `--hw` | `HW_ACCEL` | `auto` | Initial `hardware` setting on first run: auto, cpu, nvenc, qsv, vaapi, amf, videotoolbox |
| `--library` | `LIBRARIES` (comma-sep) | none | Libraries to create on first run (convenient for compose files) |
| `--log-level` | `LOG_LEVEL` | `info` | also honors `RUST_LOG` |
| `--dev-cors` | `DEV_CORS` | false | Permissive CORS for `next dev` on another port |

## Database (SQLite, WAL)

Migrations are versioned with `PRAGMA user_version`. Version 1 is this schema.
A pre-v1 database (tables `media_files`/`library_paths` from the prototype) is
dropped and recreated; it only ever held re-scannable data.

```
settings(key TEXT PK, value TEXT JSON)              -- one row: key='settings' → Settings JSON; key='queue_paused'
libraries(id TEXT PK, name, path UNIQUE, enabled INT, profile TEXT JSON,
          last_scan_at TEXT NULL, created_at TEXT)
files(id TEXT PK, library_id FK→libraries ON DELETE CASCADE, path UNIQUE, relative_path,
      file_name, size_bytes INT, modified_at TEXT, status TEXT, probe TEXT JSON NULL,
      container, video_codec, audio_codec, resolution, hdr, duration_secs REAL, bit_rate INT,
      original_size_bytes INT NULL, saved_bytes INT NULL, skip_reason, error,
      job_id TEXT NULL, scanned_at, updated_at)
      INDEX(library_id, status), INDEX(status)
jobs(id TEXT PK, file_id FK→files ON DELETE CASCADE, library_id, file_name, file_path,
     state TEXT, stage TEXT, priority INT, progress REAL, fps REAL, speed REAL, eta_secs INT,
     encoder, hw_api, attempt INT, input_size INT, output_size INT, error, skip_reason,
     validation TEXT JSON, command, log_tail, created_at, started_at, finished_at)
     INDEX(state, priority DESC, created_at)
activity(id INTEGER PK AUTOINCREMENT, at, level, message, file_id, job_id, library_id)
savings(date TEXT PK 'YYYY-MM-DD', saved_bytes INT, files INT)
```

Rules:
- Timestamps are RFC 3339 UTC strings. UUIDs are hyphenated lowercase strings.
- `files.size_bytes`/`modified_at` always describe the file currently on disk.
  After a replace they describe the new file, so rescans see it unchanged.
- Activity keeps the newest 5 000 rows (trim on insert, cheaply).

## File and job lifecycle

```
scan/watch ─► probe ─► decide(profile)
                        ├─ Skip{reason}          → file.status = skipped
                        └─ Transcode             → pending, or queued + job(queued) if auto_queue
dispatcher claims job ─► running(preparing ► transcoding ► verifying ► finalizing)
   ├─ Done     → file: done, size=new size, original_size, saved_bytes; savings[today] += saved
   ├─ Skipped  → file: skipped + reason (e.g. "Only 3% smaller — kept the original")
   ├─ Failed   → file: failed + error; original untouched
   └─ Cancelled→ file: pending (or queued again if the user hit "Stop now")
```

- Startup recovery: jobs `running` → `queued` (attempt reset), files
  `processing` → `queued`. Then every temp/backup artifact under the temp dir
  and library roots is passed to `worker::finalize::recover_artifact`.
- Changed files (size or mtime differ) are re-probed and re-decided. Files
  `done` whose size/mtime still match are left alone. Watch events for files
  that are `queued`/`processing` are ignored.
- Removed files are deleted from the DB (their jobs cascade).
- A file in `failed` is not re-queued automatically; the user retries.

## Dispatcher

- Loop woken by `Notify` (new job, settings change, job finished) plus a 5 s tick.
- Effective `max_jobs` = `settings.max_jobs` or `hardware.recommended_jobs.total`
  (and `--max-jobs` before settings exist). Changes apply immediately; running
  jobs are never killed to shrink.
- Does not start jobs when paused or outside `active_hours` (local time via `TZ`).
- Claim: pick `queued` job with highest priority, then oldest; mark running in
  the same transaction.
- Each running job has a `CancellationToken`. `POST /api/jobs/{id}/cancel`
  cancels it. `POST /api/queue/stop` cancels all running jobs and re-queues them.
- Progress from the worker: broadcast every update as `job.progress`; write to
  the DB at most every 2 s per job.
- Encoder candidates come from `hwdetect::encoder_candidates(hw, profile.video_codec,
  settings.hardware, settings.cpu_fallback)`.

## Worker behaviour (normative)

**decide** skips when: no real video stream ("Audio-only files are left as they
are"); `skip_efficient` and source codec efficiency rank ≥ target rank
("Already AV1"); not `skip_efficient` and source video codec == target and
container matches; file shorter than 1 s or unprobeable.

**build_plan**:
- Explicit `-map 0:<index>` per kept stream. Primary video only (no cover art,
  no data/timecode streams). Audio filtered by `audio_languages` (never drop all
  audio). Subtitles per `Container::subtitle_action` and `subtitle_languages`;
  attachments only for MKV (`-map 0:t?` not used; map attachment indices).
- `-map_metadata 0 -map_chapters 0`, `-max_muxing_queue_size 9999`,
  `-analyzeduration 100M -probesize 100M` on input, `-f <muxer>`.
- Audio per output stream: copy when the profile says copy (or source already in
  target codec) and the container can hold it; else encode with
  `AudioCodec::default_bitrate_kbps(channels)`, downmixing past
  `max_channels()`. Opus with >2 channels: `-mapping_family 1` and a channel
  layout normalization filter (libopus rejects `5.1(side)`).
- Video: keep 10-bit for sources with bit depth > 8 when the target supports it
  (`yuv420p10le` / `p010le`), else `yuv420p` / `nv12`. Pass color primaries /
  transfer / matrix through. HDR → H.264 is skipped by `decide` unless the
  source is SDR (no tone mapping in v1). Deinterlace (`bwdif`) interlaced
  sources (forces CPU decode). Downscale when `max_height` is exceeded. Odd
  dimensions are padded/cropped to even.
- Quality from `quality.rs`: `QualityLevel` → encoder scale (CRF for x264/x265/
  SVT-AV1/libaom/libvpx, `-cq` NVENC, `-global_quality` QSV, `-qp`/`-rc_mode`
  VA-API, `-q:v` VideoToolbox, `-qp_i/-qp_p` AMF). `quality_override` wins.
  `SpeedPreset` → encoder preset.
- HW init per API, e.g. NVENC `-hwaccel cuda -hwaccel_output_format cuda` when
  `hw_decode`; VA-API `-init_hw_device vaapi=va:<node>` (+`-hwaccel vaapi
  -hwaccel_output_format vaapi` when `hw_decode`, else `format=nv12,hwupload`);
  QSV via `-init_hw_device qsv=qs:...`; VideoToolbox/AMF accept system frames.
- MP4: `-movflags +faststart`, HEVC gets `-tag:v hvc1`.

**run_job** attempt chain: for each candidate: attempt with `hw_decode` as given;
if a hardware attempt fails, retry the same encoder with CPU decode; then move
to the next candidate. Stop at the first success. Cancellation kills ffmpeg
(SIGKILL after 5 s grace), removes the temp file and returns `Cancelled`.

**Size rule**: after encoding, if `profile.min_savings_pct = Some(p)` and the
output is not at least `p`% smaller, delete it and return `Skipped` ("Only 4%
smaller — kept the original"). Checked before verification to save time.

**validate_output** by level:
- `quick`: ffprobe parses output; stream counts match `expected`; video codec is
  the target; duration within max(1 s, 0.5%) of source.
- `standard`: quick + full decode (`ffmpeg -v error -i out -map 0 -f null -`,
  any error line = fail) + visual comparison at 4 evenly spaced 2 s segments.
- `thorough`: standard with 10 segments + black-frame and frozen-frame totals
  compared against the source (output may not add > 2 s of either).
- Visual comparison: decode the same segment from source and output, scale the
  source to the output size, align with `setpts=PTS-STARTPTS` (seek relative to
  each file's `start_time`), run `ssim` and `psnr`. Fail if any segment's SSIM
  (All) < 0.90 or mean SSIM < 0.95 (corruption, green/grey frames, blocking).
- Report every check with a plain-language label and detail.

**finalize**: temp file → final place. Replace mode: if the final path equals
the input path, rename input → backup (`paths::backup_file_name`), rename temp →
final, delete backup. If the extension changes, refuse to overwrite an unrelated
existing file, move temp → final, then delete the original. Cross-device moves
copy to a hidden temp name in the destination directory, fsync, then rename.
Preserve mtime when `keep_file_dates`, and the original's permission bits.

## REST API (`/api`)

All responses are JSON. Errors: HTTP 4xx/5xx with
`{"error": "<plain sentence>", "code": "<snake_case>"}`.
List endpoints return `{"items": [...], "total": <n>}`.

| Method & path | Body / query | Returns |
|---|---|---|
| `GET /health` | | `{"ok":true,"version":"0.2.0"}` |
| `GET /overview` | | `Overview` |
| `GET /libraries` | | `Library[]` |
| `POST /libraries` | `{"path", "name"?, "profile"? , "goal"?}` | `Library` (201). 400 `path_not_found`/`not_a_directory`/`not_readable`, 409 `library_exists` or `library_overlaps` (nested inside another library). Starts a scan. |
| `GET /libraries/{id}` | | `Library` |
| `PATCH /libraries/{id}` | `{"name"?, "enabled"?, "profile"?}` (profile is normalized; response includes it) | `Library`. Profile changes re-decide `pending`/`skipped` files (not done/failed). |
| `DELETE /libraries/{id}` | | 204. Removes DB rows only, never media. Cancels its running jobs. |
| `POST /libraries/{id}/scan` | | 202 `{"started":true}` (409 if already scanning) |
| `POST /scan` | | 202, scans all enabled libraries |
| `GET /files` | `status`, `library`, `q` (substring of name/path), `sort` (`name`,`size`,`updated`,`status`; prefix `-` for desc), `limit` (≤500, default 100), `offset` | `{"items": MediaFile[], "total"}` (no `probe`) |
| `GET /files/{id}` | | `{"file": MediaFile (with probe), "jobs": Job[] (newest first, ≤10)}` |
| `POST /files/{id}/queue` | `{"priority"?}` | `Job`. Works for pending/failed/skipped/done (re-encode). 409 if queued/processing. |
| `POST /files/{id}/skip` | | `MediaFile` status skipped, reason "Skipped by you"; cancels queued job |
| `POST /files/bulk` | `{"action":"queue"\|"skip"\|"retry_failed", "ids"?: [], "library"?, "status"?}` | `{"affected": n}` |
| `GET /jobs` | `state` (`active` = queued+running, `running`, `queued`, `history` = finished), `limit`, `offset` | `{"items": Job[], "total"}`; active sorted running-first then queue order; history newest first |
| `GET /jobs/{id}` | | `Job` |
| `POST /jobs/{id}/cancel` | | `Job` |
| `POST /jobs/{id}/priority` | `{"priority": int}` or `{"move":"top"}` | `Job` |
| `POST /jobs/clear` | `{"state":"history"}` | `{"affected": n}` deletes finished job rows (files keep status) |
| `GET /queue` | | `QueueState` |
| `POST /queue/pause` / `POST /queue/resume` | | `QueueState` |
| `POST /queue/stop` | | `QueueState` (cancel running, re-queue them, pause) |
| `GET /settings` | | `Settings` |
| `PATCH /settings` | partial `Settings` JSON (merged at top level; `default_profile` replaced whole) | `Settings`. 400 on invalid (e.g. folder mode without folder, unwritable temp dir) |
| `GET /hardware` | | `HardwareInfo` |
| `POST /hardware/detect` | | `HardwareInfo` (re-runs detection, ~seconds) |
| `GET /presets` | | `{"goals": [{"goal","title","summary","profile"}], "video_codecs": [{"codec","label","royalty_free","hw_accelerated": bool, "encoders": [names verified]}], "audio_codecs": [{"codec","label"}], "containers": [{"container","label","video": [...], "audio": [...]}]}` |
| `GET /fs/browse` | `path` (default: first browse root) | `{"path","parent": string\|null,"roots": [string],"entries":[{"name","path","is_dir":true,"media_count"?: n}]}` directories only, sorted, hidden dirs excluded; 403 outside roots |
| `GET /activity` | `limit` (≤500, default 100), `before` (id) | `{"items": ActivityEntry[]}` newest first |
| `GET /ws` | WebSocket | `Event` JSON messages (see `core::event`) |

WebSocket: on connect the server sends `queue.state` and `stats.updated`
immediately. Server pings every 30 s. A lagging client just misses events;
clients refetch on reconnect.

## Hardware detection (normative)

- CPU: model from `/proc/cpuinfo` (fallback: "Unknown CPU"), logical cores via
  `available_parallelism`, cgroup v2 `cpu.max` / v1 `cpu.cfs_quota_us`.
- Memory: `/proc/meminfo` + cgroup `memory.max` / `memory.limit_in_bytes`.
- GPUs: `/sys/class/drm/renderD*/device/{vendor,driver}` (0x8086 Intel,
  0x1002 AMD, 0x10de NVIDIA); names from `lspci -mm` if present, else
  `/sys/.../device/product_name`/`label`, else vendor + node. NVIDIA via
  `nvidia-smi --query-gpu=name,driver_version --format=csv,noheader` and
  `/proc/driver/nvidia/gpus`. On macOS: VideoToolbox assumed present.
- Encoders: parse `ffmpeg -hide_banner -encoders`; then for each hardware
  encoder listed, run a 1-second 256x256 test encode (10-bit where the API
  supports it is not required) with the same init flags `build_plan` uses,
  timeout 15 s, in parallel per API. Software encoders listed are `verified`
  without a test.
- Hints (plain language, with copy-paste fixes): ffmpeg missing; NVIDIA device
  visible but NVENC fails → `--runtime=nvidia` + `NVIDIA_VISIBLE_DEVICES=all`
  + `NVIDIA_DRIVER_CAPABILITIES=all` (Unraid: install the Nvidia-Driver plugin);
  `/dev/dri` missing → `--device=/dev/dri`; render node present but permission
  denied → group/PGID advice; Intel GPU without QSV → `intel-media-driver`;
  no GPU at all → CPU encoding is fine, here is the expected speed.
- `recommend_jobs`: CPU jobs = clamp(floor(effective_cores / 4), 1, 8), further
  capped by memory (1.5 GB per job) — effective cores honor cgroup limits.
  GPU jobs: NVIDIA 3 per GPU (consumer NVENC session limits), Intel/AMD 2 per
  GPU, Apple 2; capped by effective cores. `total` = GPU jobs when a verified
  hardware encoder exists for any codec and preference ≠ cpu, else CPU jobs.

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
   resolution breakdown.
3. **Queue**: Running / Up next / History (done, skipped, failed) with cancel,
   move to top, retry, and a detail sheet (streams before→after, verification
   report with SSIM, ffmpeg command, log tail).
4. **Library** (per library): file table with search, status filter, sort,
   pagination, bulk actions; library settings (goal, quality, advanced).
5. **Settings**: Processing (jobs at once: Automatic (n) or a number; active
   hours; auto-queue; watch folders), Output (replace vs folder; temp folder;
   keep dates), Verification level, Hardware (detected devices, verified
   encoders, hints, re-detect, preference), Advanced (ignore patterns, min size,
   default profile).

Plain language first: say "Smaller files" not "CRF 32"; show codecs as
secondary detail; every destructive action is reversible or confirmed.

## Deployment

- `Dockerfile`: multi-stage (Rust build, web export, runtime on
  `debian:bookworm-slim` + `jellyfin-ffmpeg7`), multi-arch amd64/arm64.
  Entrypoint handles `PUID`/`PGID`/`UMASK` (Unraid defaults 99/100), adds the
  user to the groups owning `/dev/dri/*`, then drops privileges.
- Volumes: `/config` (DB), `/media` (libraries), `/temp` (optional scratch,
  put it on an SSD/cache pool). One port: 8080.
- GPU: NVIDIA via `--runtime=nvidia` (Unraid Nvidia-Driver plugin) or compose
  `deploy.resources.reservations.devices`; Intel/AMD via `--device=/dev/dri`.
- `unraid/chrysopoeia.xml`: Community Applications template with those fields.
- `docker-compose.yml` (CPU), `docker-compose.nvidia.yml`,
  `docker-compose.intel-amd.yml` overlays.
