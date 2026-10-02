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
| `chrysopoeia-worker` | `plan`/`quality`: skip decision + ffmpeg args. `ffmpeg`/`run`/`validate`/`finalize`: process execution with fallback chain, verification, crash-safe replacement | `decide`, `decide_forced`, `build_plan`, `run_job`, `validate_output`, `finalize::*` (incl. `resume_replace`, `remove_backup`, `recover_artifact`), `slow_fs` |
| `chrysopoeia-server` | Config (CLI/env), SQLite schema + migrations, REST + WS, LibraryService, Dispatcher, static UI hosting, filesystem browser | binary `chrysopoeia` |

Existing public signatures are fixed; new public items may be added. The
workspace's minimum Rust version (MSRV) is 1.88.

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
| `--max-jobs` | `MAX_JOBS` | unset | 1–32 or `auto`. Stands in for the automatic job count whenever `settings.max_jobs` ("Files at once" in the UI) is null (`QueueState.max_jobs_source` is then `env`); a number saved in Settings wins, and the activity feed says so at start, as one warning ("MAX_JOBS=3 is not used because Files at once is set to 2 in Settings. Choose Automatic there to use MAX_JOBS.") |
| `--hw` | `HW_ACCEL` | `auto` | auto, cpu, nvenc/nvidia, qsv/intel, vaapi, amf, videotoolbox, rkmpp, v4l2m2m. Sets the `hardware` setting on the first run and again whenever the value changes; otherwise the Settings choice is kept |
| `--library` | `LIBRARIES` (comma-sep) | none | Libraries created on the first run |
| `--allowed-host` | `ALLOWED_HOSTS` (comma-sep) | none | Extra host names (e.g. a reverse proxy's domain; `.example.com` covers a domain; `*` = any) |
| `--log-level` | `LOG_LEVEL` | `info` | `error`, `warn` (or `warning`), `info`, `debug` or `trace`, in any case; anything else stops the start with a plain message (an unknown word would otherwise silence every log line). `RUST_LOG` (full filter directives) wins when set |
| `--dev-cors` | `DEV_CORS` | false | Permissive CORS and no origin check, for `next dev` on another port |

Not configurable: a file must go 20 s without changes (size and mtime) before
it is picked up or converted (the settle time); the database busy timeout is
30 s. Active hours use the local time zone (`TZ`).

Start order: raise the soft limit on open files to the hard limit (at most
65 536; Docker often starts with 1 024) → exclusive lock on
`$DATA_DIR/chrysopoeia.lock` (a second
Chrysopoeia on the same data folder exits with "Another Chrysopoeia is already
using the data folder …" and changes nothing, advising to stop the other one or
give this one its own Config folder in a container (the image sets the data
folder there), and `--data-dir` / `DATA_DIR` outside one; filesystems that
can't lock only log a warning) → bind the port → open the database → recovery.

A database file SQLite finds damaged (`SQLITE_CORRUPT`, `SQLITE_NOTADB`) is
moved aside as `chrysopoeia.db.damaged-<UTC time>` (with its `-wal`/`-shm`
files) and a new one is started; the feed says so at WARN ("The database was
damaged, so Chrysopoeia moved it aside to … and started with a new one. Your
media files were not touched. Add your libraries and settings again."), so a
container set to restart doesn't loop. A data folder on a full disk stops
the start with "The disk that holds the data folder (…) is full, so the
database there couldn't be opened. Free some space on that disk, then start
Chrysopoeia again" (`db::is_disk_full`: SQLite's `SQLITE_FULL` or the
system's "no space left", or any database error while less than 1 MiB is
free there; a new database fails with the former, an existing one with a
disk I/O error). This covers opening, migrating and the first writes of the
start. Other open errors still stop the start with the data-folder advice.

At stop, after the HTTP server and the jobs have wound down (jobs get 7 s;
one putting its new file in place on a hung share is left to the next
start, see "Putting the new file in place on a share that stops
answering"), the process waits at most 2 s for work still on blocking
threads (a system call stuck on a share that stopped answering never
returns), so `docker stop` ends cleanly. A killed ffmpeg that doesn't end
within 2 s (one with files on such a share can't end until it answers:
closing a file there waits for it) is left to end by itself, and cleaning
up after a stopped job waits at most 2 s before it is left to the
background.

**HTTP server** (`http.rs`, hyper with a timer instead of `axum::serve`): a
request's headers must arrive within 30 s (which also closes a kept-open
connection idle that long), at most 256 connections are served at once (a
WebSocket keeps its place while open; further connections wait in the
system's queue), and a WebSocket client may send messages of at most 64 KB.
Starting ffmpeg/ffprobe while too many files are open (`EMFILE`/`ENFILE`) is
retried for about 30 s instead of failing the file.

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
      problem TEXT NULL, job_id TEXT NULL, verdict_profile TEXT JSON NULL, scanned_at, updated_at)
      INDEX(library_id, status), INDEX(status)
jobs(id TEXT PK, file_id FK→files ON DELETE CASCADE, library_id, file_name, file_path,
     state TEXT, stage TEXT, priority INT, progress REAL, fps REAL, speed REAL, eta_secs INT,
     encoder, hw_api, attempt INT, input_size INT, output_size INT, error, problem TEXT NULL,
     skip_reason, validation TEXT JSON, command, log_tail, notes TEXT JSON NULL,
     created_at, started_at, finished_at, final_path TEXT NULL, force INT DEFAULT 0,
     freed_bytes INT NULL, profile TEXT JSON NULL, placing INT DEFAULT 0,
     placing_size INT NULL, placing_original_size INT NULL, final_mount TEXT JSON NULL)
     INDEX(state, priority DESC, created_at), INDEX(file_id, created_at), INDEX(created_at),
     partial INDEX(finished_at) of finished jobs, partial INDEX(placing) of marked jobs
activity(id INTEGER PK AUTOINCREMENT, at, level, message, file_id, job_id, library_id,
         problem TEXT NULL)
savings(date TEXT 'YYYY-MM-DD', library_id FK→libraries ON DELETE CASCADE, saved_bytes INT,
        files INT, PK(date, library_id))
library_mounts(library_id FK→libraries ON DELETE CASCADE, path, PK(library_id, path))
folder_mounts(folder, mount, fstype NULL, source NULL, root NULL, PK(folder, mount))
```

Versions: 1 = base schema; 2 = job-list indexes and `finished_at` on every
finished job; 3 = `jobs.notes`; 4 = `jobs.final_path` (where a started job
puts its result, written when it starts); 5 = `jobs.force` ("Convert
anyway"), `libraries.settling` (files still being copied, see
`LibraryStats.settling`) and `savings` keyed by day and library. The old
daily rows can't be split by library and may include libraries removed
before (even when one is left), so the history is rebuilt from the
finished conversions on record; each file's latest conversion is kept by
trimming, so they cover the 30 days shown (unless the history was cleared).
6 = `jobs.problem` and `files.problem` (see Problems below). Errors recorded
before are sorted by the wording that version used (`problem_from_error` in
`db/migrate.rs`): a missing or moved file → `source_changed`; a damaged,
unreadable or slow-to-read original or no read permission (also as the last
of several attempts) → `unreadable_source`; a failed check → `verification`;
a full disk or quota → `disk_full`; an unusable temp folder →
`work_folder`; refused hardware or no working encoder →
`hardware_unavailable`; no write permission, a name already taken, no
output folder or a failed move → `destination`; an encoder that stopped,
hung or wrote nothing → `encoder`; the rest become `other`.
7 = portrait videos relabelled: `files.resolution` of a picture taller than
wide is the class of the same picture turned sideways (1080×1920 is
`1080p`, not `1440p`, as `core::media::resolution_label` now gives it and
as the size limit judges it). 8 = `library_mounts`: drives and shares
mounted inside a library folder that scans have seen (see File and job
lifecycle). 9 = `jobs.freed_bytes` (the space a conversion released, not
guessed for conversions finished before) and `activity.problem` (the kind of
problem an entry about a failed file is about; the error entries already in
the feed take it from the failed job they refer to); see Contract additions
(round 5). 10 = `files.verdict_profile` (the goal a `pending` or `skipped`
verdict was decided with, see "Verdicts and goal changes") and
`jobs.profile` (the goal a job ran with, written when it starts, with
`final_path`; `NULL` for jobs from before). Verdicts recorded before are
taken to be the library's current goal's, except a file a job skipped by
the size rule in a library whose goal has no size rule now (an earlier
goal's verdict): it is left unknown, so the next scan decides it again.
11 = `jobs.placing`, `jobs.placing_size` and `jobs.placing_original_size`:
a job whose new file may have been put in place after it ended (1), or
was found in place with only the backup of its original left to remove
(2), with the sizes of the new file and of the original when known; see
"Putting the new file in place on a share that stops answering". Such
jobs are kept by history trimming and clearing until that is settled.
12 = `folder_mounts`: the mount points each library folder, the output
folder and the work folder were seen on or under (see "Drives and shares
the folders sit on" under File and job lifecycle). Nothing is known for
the folders in use before: they are learned from the next look at them.
13 = what was mounted at each remembered mount point
(`folder_mounts.fstype`, `source` and `root`, as `/proc/self/mountinfo`
gives them), so another filesystem mounted at that place isn't taken for
the share, and `jobs.final_mount` (JSON `point`, `fstype`, `source`,
`root`): the mount the folder a job's new file goes into was on when it
started. The mount points remembered before keep no note of what was
mounted there until a look finds something mounted there: at start-up
(before anything looks at a folder), every 15 s after while any are left
(`share_mounts::note_unknown`), and at every look at their folder. Until
then nothing mounted there is not connected, and the folder below bound
onto itself (see "Drives and shares the folders sit on") is another drive,
never noted. Another filesystem mounted there before that first look (a
tmpfs, or a host folder bound into a container) can't be told from the
share and is noted as it; the user then says which one to use as below.

Rules:
- Timestamps are RFC 3339 UTC strings with milliseconds. UUIDs are hyphenated
  lowercase strings. Write transactions use `BEGIN IMMEDIATE`.
- A connection whose transaction ended badly is closed, never reused: when a
  write fails because the disk is full (or on an I/O error), SQLite rolls
  the transaction back itself and sqlx would keep counting the connection
  as inside one, so every later write would fail until a restart. The pool
  checks each connection when it is returned and before it is handed out,
  and `write_tx` tries another connection if one slips through.
- `problem` goes with `error`: every write that sets a file's or job's
  `error` sets its `problem`, and every write that clears the error (queued
  again, converted, skipped, kept as converted) clears it. Reads enforce the
  same (`other` for an error stored without a kind, none without an error).
- `files.size_bytes`/`modified_at` always describe the file currently on disk.
  After a replace they describe the new file, so rescans see it unchanged.
- Activity keeps the newest 5 000 rows. Finished jobs are trimmed to the
  newest 5 000 and 90 days, except the job each file points at and the
  latest conversion of a converted file that is queued again.
- A file converted before (`original_size_bytes` set) whose new job ends
  without a new result (skipped, cancelled, removed from the queue, or
  failed for any reason but the file being gone) stays `done` with its
  savings and points at its latest conversion again (`files.job_id`); the
  ended job stays in the history as the record of that attempt. When a
  job finds the file's content changed, its earlier savings are cleared
  first (it is a different file now).
- Savings rows belong to a library and go with it, so the savings chart
  (`Overview.savings_history`) and the total above it
  (`Overview.totals.saved_bytes`, the files listed now) describe the same
  libraries.

## File and job lifecycle

```
scan/watch ─► probe ─► decide(profile)
              │         ├─ Skip{reason}          → file.status = skipped
              │         └─ Transcode             → pending, or queued + job(queued) if auto_queue
              └─ can't be read → file: failed + error, problem unreadable_source
                                 (other when ffprobe itself can't run)
dispatcher claims job ─► running(preparing ► transcoding ► verifying ► finalizing)
   ├─ Done     → file: done, size=new size, original_size, saved_bytes;
   │             savings[today, library] += saved
   ├─ Skipped  → file: skipped + reason (e.g. "Only 3% smaller — kept the original");
   │             activity "Skipped <name>: <reason>"
   ├─ Failed   → file: failed + error + problem; original untouched
   └─ Cancelled→ file: pending (queued again after "Stop now" or shutdown; skipped after "Skip")
   A file converted before (original_size set) stays done with its savings
   when its new job is skipped, cancelled, removed from the queue or fails;
   only the job row records that attempt.
```

- Scans: walk (blocking thread) → compare with the DB by path, size and mtime
  → probe new/changed files (4 at a time, 60 s each) → decide with the
  profile read right before each batch of 50 is written. Files still settling
  are looked at again later; their number is stored on the library at the
  end of each scan and lowered as they settle (a later scan of the library
  takes over the count). The folder watcher's count of media files still
  being written (`LibraryWatcher::waiting_files`, polled every 2 s) is kept
  beside it, and `LibraryStats.settling` is the larger of the two (both
  usually see the same copies); the UI shows "Waiting for N files to
  finish copying" from it. A library that still had such files when
  the server stopped is scanned again at start. Removed files are deleted
  from the DB (their jobs cascade), except below folders the walk couldn't
  read or left alone, and never when a library that had files walks empty
  (an unmounted share). A file that is still on disk is never removed. A
  folder whose listing fails part way counts as not read (its files
  stay), and a file whose probe failed is recorded as unreadable only when
  a look at it then answers: one that is gone, doesn't answer, or answers
  with an error (a share answering with errors) is left to the next scan.
- Drives and shares mounted inside a library folder: each scan reads the
  mount points under the library from `/proc/self/mountinfo` and remembers
  them (`library_mounts`). One that is no longer mounted and whose folder is
  empty or missing counts as offline: its files stay listed with their state
  ("Skipped by you" included), and the feed says once that "the drive or
  share mounted there isn't connected, so its files stay in the list until
  it's back". It is forgotten once its folder has files without being a
  mount. (When the folder doesn't answer, every known mount counts as
  offline.)
- Drives and shares the folders sit on (`services::share_mounts`). An
  unmounted share leaves its mount point behind as an ordinary folder on
  the disk below: empty, or holding whatever was there before the share
  was mounted over it; and another filesystem may be mounted in its place
  (a tmpfs, or the bare folder bind-mounted onto itself, which is what a
  Docker bind mount shows when the container started before the host
  mounted the share: on Unraid, a remote share Unassigned Devices mounts
  after the array started). So the mount points each library folder, the
  output folder and the work folder sit on or under (from
  `/proc/self/mountinfo`, read without touching any share; not `/`, and
  not a share an automounter mounts on demand on an `autofs` mount, which
  comes back by itself when looked at) are remembered (`folder_mounts`)
  with what is mounted there: its filesystem type, source and root (the
  folder of that filesystem mounted there; a bind mount of a folder names
  it). Not the mount's id or device number: both are handed out again to
  whatever is mounted next. So another share mounted at that place with
  the same type, source and root is taken for the usual one: for NFS and
  SMB the source names the share, so that is the same share. Of several
  mounts at one place the one on top (the one the folder shows) counts.
  The folder below bound onto itself (a mount of the filesystem the
  folder above it is on, the same type and source, with the folder at
  that place as its root, or that filesystem as a whole;
  `slow_fs::shows_folder_below`) is never taken for a share remembered
  without what was mounted there (by an older version, see Database 13):
  it is another drive. A host folder bound into a container comes from
  another filesystem than the container's, so it can't be told from a
  share this way. A folder given as a link is followed
  (`slow_fs::real_path`, a bounded check with a 5 s limit; the last answer
  is used, and found out again in the background once a minute old), so
  the share a link leads to is remembered too; and for the output folder,
  drives and shares mounted inside it (`out/Movies` on a share of its own)
  are remembered with it. Any found mounted is added whenever the folder
  is looked at: when a library is added, when the output or work folder
  is chosen in Settings, at start-up, and by every library view, scan and
  job. While one of them isn't mounted, the folder is not connected ("The
  drive or share mounted at /mnt/remotes/nas isn't connected. Reconnect
  it, and its conversions continue."), and while something else is
  mounted there it is not connected either ("A different drive is mounted
  at /mnt/remotes/nas than before. Reconnect the usual one, or tell
  Chrysopoeia to use the one there now."), whatever the folder holds: the
  library shows that as its `path_error` (with `changed_mount`, the place,
  for another drive), a scan of it stops there (an error entry, nothing
  taken for removed or added), its jobs and every job that uses the
  output or work folder wait with their library offline (checked again
  every 15 s; nothing is read from or written into the mount point), the
  start-up search for leftovers leaves it for later, and a job whose new
  file may have been put in place is never settled on it. Once the share
  is mounted again (the same filesystem, source and root; the mount's id
  may differ), it is connected. Each job also notes the mount the folder
  its new file goes into is on when it starts (`jobs.final_mount`, with
  `final_path`; links followed): the worker writes nothing once that isn't
  mounted as it was, and a job whose placing isn't settled is only settled
  on it (its library waits for it meanwhile).
  Another drive put there on purpose is taken as the usual one when the
  user says so: "Use the drive that's there now" on the library's page
  (`POST /api/libraries/{id}/relearn-mounts`) takes, for the folders the
  library's jobs use (its folder, the work folder, the output folder) and
  for the mounts its unsettled conversions go into, what is mounted now at
  every place with something else mounted than before as the usual one
  (a place with nothing mounted stays not connected), and checks the
  waiting libraries again at once. Settings shows the same for the output
  and work folders where they are chosen (`GET /api/settings/folders`),
  with the same action for them (`POST /api/settings/relearn-mounts`, which
  also takes the drive for the unsettled conversions whose new file goes
  there). Saving Settings learns a folder's drives afresh only when the
  folder changes: another place, links followed (`share_mounts::same_folder`,
  each path found out within 5 s). Saved again as it was (picked again, or
  sent with another setting), or given another way that leads to the same
  place (a link), it keeps what was remembered (carried over to the new
  way), whatever is mounted there now: with the share unmounted or another
  drive in its place it stays not connected, and isn't written into to
  check it.
  Mounts are only forgotten when a folder stops being used for that, so
  a share removed for good is moved off like this: a library is removed
  and added again (its mounts are learned afresh from that moment), or
  the output or work folder is changed in Settings (the folder it replaces
  is forgotten; choosing the old one again later learns it afresh, as an
  ordinary folder when nothing is mounted there).
- Leftovers in library folders: every scan hands the temp and backup files
  its walk finds (`WalkResult.artifacts`) to `recover_artifact`, except
  those of running jobs and of jobs whose new file is still being put in
  place: a temp file left in a folder renamed during a conversion is
  deleted, and an original a crash left moved aside is put back (and stays
  listed). The files of a job marked as one whose new file may have been
  put in place after it ended (`jobs.placing`) are never handed over
  blindly: such a job that can't run again by itself (cancelled, say) is
  settled first (`dispatcher::settle`: the disk tells whether its new file
  got there, see "Putting the new file in place on a share that stops
  answering"), and one in the queue is left alone until it settles itself
  when it runs. Either way its original is not taken for removed, and nor
  is the file of any marked job (its job records what became of it). A job that fails after its conversion started
  and whose file is gone from its folder (a folder renamed meanwhile; the
  converter then finds no new file at the old path) has the library
  searched for its own temp files at once.
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
- Every scan ends by deciding again the `pending` and `skipped` files whose
  verdict came from another goal than the library's current one, or from
  an unknown one (see "Verdicts and goal changes").
- Files `queued`/`processing` are not touched by scans or watch events. Files
  `done` whose size/mtime still match are left alone. A file in `failed` is
  not re-queued automatically; the user retries.
- Startup recovery (`dispatcher::complete_interrupted`), before anything
  else looks at the disk: jobs left `running` that may have been putting
  their new file in place are marked (`jobs.placing` = 1: replacements with
  a `final_path`, and in folder mode those at the `finalizing` stage), and
  every marked job (these, and those a stop left putting their new file in
  place after the job ended, whatever the stop recorded them as: back in
  the queue, cancelled) is settled from what is on the disk (see "Putting
  the new file in place on a share that stops answering"): its new file in
  place → recorded `done` (note: "Chrysopoeia stopped just as the new file
  was being put in place. The new file was already complete, so it was
  kept") and the backup removed; not in place → the original put back and
  the job's temp files gone. Only an answer settles a job: a folder that
  doesn't answer within 10 s, answers with an error (a soft-mounted share
  that timed out, a FUSE share whose server stopped), or isn't there as it
  should be (its library folder missing or empty, a share mounted in the
  library disconnected, the library or output folder on a share seen
  mounted there that isn't now or has another drive in its place, the
  mount its new file went into not mounted as it was when the job
  started) leaves its job marked and touches nothing: the job
  settles itself when it runs again (before
  it does anything with its file), a job that can't run again is settled
  every 15 s in the background, and a search for leftovers that finds its
  files settles it first. Two marked jobs aiming at one name can't be told
  apart: both originals go back. Then jobs `running` → `queued` (attempt
  reset), files `processing` → `queued`, and the other such jobs' own temp
  and backup files (named with its id) in its file's folder and its
  destination folder are recovered right away: an original moved aside is
  put back before the job runs again. (A job that starts and finds its file
  gone looks for its own backup once more, for a folder that was out of
  reach at the start.) Then every temp/backup artifact is passed to
  `worker::finalize::recover_artifact` (those of marked jobs as in the
  leftovers rule above): in the temp folders always, and in the library
  folders and the output folder after an unclean shutdown. A stop is
  recorded as clean only when no job was running, no new file was still
  being put in place, and this search was done everywhere: a stop during
  it, or a library or output folder that was out of reach (missing, not
  responding, on a share that isn't mounted, or a library with files that
  is empty now), keeps it due, and
  a scan that reaches such a library does the search there.

## Verdicts and goal changes

- Every automatic verdict (a scan's or watch event's `pending`/`skipped`, a
  job's skip, a re-decision) records the goal it was decided with
  (`files.verdict_profile`, the profile's JSON, compared by value). A
  status set without a decision (a cancelled job, a file taken out of the
  queue) forgets it, so the next scan decides that file.
- A goal change (`PATCH /libraries/{id}`) and the end of every scan decide
  again the `pending` and `skipped` files whose verdict's goal isn't the
  current one (files skipped by the user stay skipped). A file a job
  skipped by the size rule stays skipped, with the current goal as its
  verdict's, unless the change affects the output size (codec, audio,
  quality, speed, size limit, minimum saving, audio languages), judged from
  its verdict's goal, else the goal before the change; with neither, it is
  decided again. Skipped files the goal now converts are queued (`pending`
  without auto-queue); pending ones stay pending. Each transaction re-reads
  the library's goal and stops when it changed again meanwhile (that
  change decides the files itself).
- A job runs with the goal read when it starts (`jobs.profile`). When it
  ends under another one (the goal changed while it ran, once or more), its
  file is decided again against the current goal in the transaction that
  records the end:
  - skipped, or failed by the goal's own rule (`hardware_unavailable`: the
    chosen hardware can't make its codec): queued again when the current
    goal converts it (`pending` without auto-queue), else skipped with the
    current goal's reason; a size-rule skip stays (current goal recorded)
    when the change doesn't affect the output size;
  - converted, or kept as converted (`done`), in replace mode and with
    auto-queue on: queued again when the current goal would convert the
    new file as it is (from its fresh probe; the usual rules skip files
    already in an efficient format); otherwise it stays `done`. In folder
    mode it stays `done`: its result is in the output folder already, and
    converting it again would need that name;
  - other failures stay failed (the user retries), and a cancelled or
    requeued job changes nothing (a requeued job runs with the current
    goal; a cancelled one's file is decided by the next scan).
  "Convert anyway" applies to the goal it was given: the new decision uses
  the usual rules and creates a normal job. A job that ran with the current
  goal (or whose goal isn't known) is never decided again, so nothing
  loops. The feed says what happened ("The goal of Movies changed while
  a.mkv was being converted, so it was queued again for the new goal." /
  "… Under the new goal it is left as it is: Already H.264 in an MP4 or
  MOV file.").

## Folder watching

- One kernel watcher per library root, armed from a blocking thread; roots are
  watched with `watch_with_options(root, ScanOptions::from_settings)` and
  watched again when `ignore_patterns` or `min_file_size_mb` change. No lock
  is held across a watcher call.
- A new or changed file is reported once no event arrived for it for the
  settle time and its size/mtime held still that long; removals (files and
  folders) are reported at once, except renames to Chrysopoeia's backup names
  and the loss of a whole root.
- Chrysopoeia's own results are not copies in progress. A converted file put
  in place goes through the same settle wait as any new file, but it is
  complete, so the count of files still being copied
  (`LibraryStats.settling`, "Waiting for 1 file to finish copying") leaves
  out the result of a running job and of a job finished within the settle
  time plus 30 s (`jobs.final_path`, one query per look at a non-empty
  waiting list; the scanner's `WaitingFiles::files()` names the waiting
  files). A watch event for the result of a job that is still running is
  left to the job: it is not probed, and under a new name it does not
  become a second row. The event for a result already recorded finds its
  size and modification time unchanged and does nothing.
- Renames and moves (a watcher removal plus a new file, or a scan that finds
  a row's file gone and a new one): a new file with the same size and
  modification time as a row removed from the same library within the last
  hour takes over that row's status, probe, reasons and savings (not its job
  history), so it is neither probed nor converted again. Queued files and
  files whose last job found them missing are decided afresh.

## Dispatcher

- Loop woken by `Notify` (new job, settings change, job finished) plus a 5 s tick.
- Effective `max_jobs` = `settings.max_jobs`, else `MAX_JOBS`, else
  `hardware.recommended_jobs.total` (automatic), clamped to 1–32.
  `QueueState.max_jobs_source` (`settings` | `env` | `auto`) says which, so
  the UI can show e.g. "Automatic (3, from MAX_JOBS)". Changes apply
  immediately; running jobs are never killed to shrink.
- While the limit is automatic, jobs whose first-choice encoder is software
  are capped at `recommended_jobs.cpu_jobs` at once (the automatic limit may
  follow the GPU count); other libraries' GPU jobs still fill the free slots.
- Does not start jobs before hardware detection and crash recovery finished,
  when paused, or outside `active_hours`.
- Claim: the `queued` job with the highest priority, then oldest, of an
  enabled library; marked running in the same transaction.
- Jobs of a library whose folder is offline (missing, unreadable, empty,
  or on a share that isn't mounted), and jobs whose work or output folder
  is on a share that isn't mounted (see "Drives and shares the folders sit
  on"), wait, and the folder is checked again every 15 s (with what stopped
  answering in it, if that was something else: "not found" is an answer,
  an error is not, so a share that answers with errors stays offline); a
  job whose file changed moments ago waits for it to settle.
- **A share that stops answering** (an NFS hard mount whose server went
  away, a stuck FUSE mount) never holds a slot for good, whenever it stops:
  every look at a library, work or output path on a job's way is bounded,
  and when one gets no answer the job goes back to the queue with its
  library offline ("The folder … isn't responding…"; not failed, and its
  file not taken for deleted), so other libraries' queues move. The bounds:
  30 s for the job's look at its file when it starts (and when it ends
  having failed: a failure while the share hangs is not recorded as one,
  and a file that answers "gone" while its library folder answers is taken
  for moved or deleted as before), 30 s for each of the worker's checks
  (the destination, folder permissions, creating the temp folder, free
  space, the temp file, the original's identity), and while ffprobe, the
  encode, the checks or putting the new file in place run, their files and
  folders are looked at every 5 s and must answer within 30 s (ffmpeg is
  then stopped well before its own 10 min stall timeout). When what stopped
  answering is not the library folder itself (one file in it, the work
  folder, the output folder), the library waits until that answers too.
  A look that couldn't be made because too many checks of other shares are
  stuck (`NoAnswer::Busy`, below) says nothing about the job's files: the
  job goes back to the queue and is tried again 15 s later, its library
  not taken for offline (`Requeue::ChecksBusy`, `JobOutcome::ChecksBusy`).
  Every await before the encode starts (the look at the file, ffprobe,
  leftover searches, the library folder check) gives way to Cancel and
  Stop, so they end such a job within seconds (the encode and the checks
  already did).
- A cancel recorded for a job (Cancel, Skip, removing its library) wins
  over putting it back in the queue: a job the user cancelled ends
  cancelled even when its share stopped answering in the same moment (the
  intent is read again on every try to record the result, and
  `POST /jobs/{id}/cancel` and Skip cancel a job that went back to the
  queue while they waited for it). Cancel answers before the other
  `*.updated` events go out (they follow in the background), so a library
  on a hung share, which takes seconds to describe, doesn't slow it.
- Folder checks that may hang (`chrysopoeia_worker::slow_fs`, used through
  `services::fs_guard`): library checks, every check of a job (server and
  worker share them), leftover searches and the folder picker run on a
  blocking thread, one at a time per path, kind of check and mount (a
  caller that comes while one is running on the same mount waits for its
  answer, up to its own timeout). A share that hung, was unmounted lazily
  (`umount -l`) and was mounted again at the same place is another mount:
  a check there starts afresh instead of waiting for the one stuck on the
  old mount, which may never answer. A check about to be refused at once
  because the checks of its mount are stuck reads the list of mounts again
  first (unless it was read in the last second), so the first look after
  such a remount already goes to the new mount, and a library on it comes
  back at its next check.
  Checks are counted by the mount they are on: the longest mount point
  above the path in `/proc/self/mountinfo` (read without touching the
  share; read again in the background every 30 s, and at most once a
  second whenever a check gets no answer in time or finds no room on its
  mount; without it, the path's first two folders). Mounts are told apart
  by their id there, so a share mounted again at one place is another
  mount. A check is counted by the mount it started on for as long as it
  runs; when the list changes, only a mount that appeared since, between
  that mount (still listed) and the check's path, takes it over: a share
  mounted since the list was read, whose checks got stuck while they were
  counted with the mount above it, gives that mount its room back. A check
  whose mount is gone from the list (a hung share unmounted lazily with
  `umount -l`, the usual way out of a hung NFS or SMB mount) stays counted
  by that mount, never by the mount above, so its stuck checks take no
  room of the healthy folders there for as long as they hang, and a share
  mounted again at that place starts with all its room. At most 8 run at
  once on one mount (`MAX_STUCK_PER_MOUNT`): once 8 there are stuck (each
  past its caller's timeout), a check of anything in the folder they have
  in common answers "not responding" at once, without a thread (so does
  one that would wait for a stuck check of the same thing); a check of a
  full mount waits for room up to its timeout, except one away from the
  folder its slow checks (stuck, or running for 2 s) have in common, which
  may take room beyond the 8 while fewer than 8 checks there are not slow:
  what is slow there is then most likely another share counted with this
  mount (one mounted since the list was read, or reached through a link),
  and must not stop the rest of the mount, a healthy library on it say,
  for as long as that share hangs, however many of its checks are stuck.
  At most 128 run at once in all (`MAX_STUCK_CHECKS`, room for 16
  hung shares, well below the runtime's 512 blocking threads); a check
  that finds no room (after waiting up to its timeout) is
  `NoAnswer::Busy`: unknown, never "not responding". A library whose
  check is busy shows no problem, an offline library stays as it was and
  is checked again 15 s later, a job waits (above), a failed job whose
  file can't be looked at is tried again rather than failed or taken for
  deleted, the folder picker answers 503 `busy` ("Chrysopoeia is still
  waiting for other folders that stopped answering, so it couldn't open
  this one right now. Try again in a moment."), and the watch over a long
  step ignores such checks. A hung share therefore costs at most 8
  threads, 8 more each time checks away from the folder its stuck checks
  have in common get stuck too (that folder then takes in more of the
  share, until it is the whole share), however many of its folders are
  looked at and however often, and never the checks of other mounts; a
  share counted with another mount (reached through a link, or mounted
  since the list was read) holds up only its own folders there.
  Reserving disk space looks at the disks without holding the reservations'
  lock, so a disk that hangs holds up no other job's reservation.
- A job that runs out of disk space (`disk_full`) while other jobs are
  running is put back in the queue once and tried again a minute later (the
  space reservation then makes it wait its turn); alone, it fails.
- Each running job has a `CancellationToken`. Cancel → file `pending` (a
  file converted before stays `done`);
  "Stop now" and shutdown → job and file back to `queued`; deleting a library
  cancels its jobs (media untouched). ffmpeg is killed at once, so
  `POST /jobs/{id}/cancel` answers within a moment with the job's final
  state; the job leaves the running set (and `queue.state`) only after its
  result is recorded. A job being put in place (finalizing) is asked to
  undo it at its next safe point (see finalize below); if that doesn't come
  within 3 s (a rename waiting for a share) the job ends anyway and the
  step goes on by itself, its file held back until it ends.
  A job the database shows as running but that has no task (it finished a
  moment ago, or a stale row) is closed directly only while it is still
  `running`, so a result recorded in the meantime is never overwritten.
- Progress: every update is broadcast as `job.progress`; the DB is written on
  stage changes and at most every 2 s per job.
- A result is never lost: recording it retries while the database is busy.
- Encoder candidates come from `hwdetect::encoder_candidates(hw, profile.video_codec,
  settings.hardware, settings.cpu_fallback)`, else the codec's software encoder.
- When `settings.hardware` names an API that can't make the library's codec
  here (`hwdetect::preference_problem`) and the file would be converted
  (`decide`, or `decide_forced` for a forced job; files the goal leaves as
  they are are skipped as usual, and no other check is pre-empted), the job
  gets a note ("You chose NVIDIA NVENC, but no working NVIDIA encoder was
  found here, so this file was converted on the CPU"); with `cpu_fallback`
  off it fails instead ("…, and converting on the CPU instead is turned
  off, so this file wasn't converted. Choose Automatic under Hardware in
  Settings, or allow CPU fallback.") and nothing is encoded. A GPU that
  only had no free encoding session when it was tested
  (`hwdetect::preference_busy`: every NVENC session taken, e.g. by Plex) is
  described as busy ("You chose NVIDIA NVENC, but its encoding sessions
  were all in use by other apps when it was checked"), and with
  `cpu_fallback` off its jobs are not failed: the job goes back to the
  queue, and the queued jobs of its library are passed over until the next
  hardware detection or a change of the hardware settings (by library, so
  a big queue isn't claimed and put back one job at a time; files there
  that need no encoding wait too). Detection then runs again every 3
  minutes for as long as jobs wait, and the feed says once, at WARN, that
  conversions are waiting.
- `jobs.force` is passed to the worker as `JobSpec.force` and echoed as
  `Job.force`.
- Two files converted to one name. In folder mode every library's folders
  are mirrored in the one output folder without the library's name (the
  layout stays so), so the same relative path in two libraries
  (`/media/movies/Movies/Frozen (2013)/Frozen.mkv` and
  `/media/kids/Movies/Frozen (2013)/Frozen.mkv`) gives one output file;
  two originals that differ only by extension do too, in either mode.
  Nothing is ever overwritten: a job starting while a job of another file
  aims at the same name (`jobs.final_path` of a running job, checked and
  taken in one transaction with `db::jobs::claim_destination`) fails at
  once, before encoding (`destination`); one that finds the name taken in
  the output folder fails as the worker says (before encoding, or when a
  file appeared meanwhile), and the error then names whose file has it
  (`finalize::output_name_taken`, `db::jobs::destination_owner`: the
  running or last finished job of another file with that `final_path`).
  Another library: "Another library's converted file, from Kids, already
  uses the name "Movies/Frozen (2013)/Frozen.mkv" in the output folder
  /out, so this file wasn't converted and that file wasn't overwritten.
  Rename one of the two files, or convert one library at a time, each to a
  different output folder chosen in Settings › Output." (while that one is
  converting: "Another library's file, from Kids, is being converted to the
  same name, … so this file wasn't converted. …"). The same library:
  "Another file in this library, "Frozen.avi", was converted (is being
  converted) to the same name, …. Rename one of the two files, then
  convert this one again." Replacing originals: "Another file in the same
  folder, "Movie.avi", is being converted to the same name, "Movie.mkv",
  so this file wasn't converted. …". When no such job is on record (a file
  someone else put there), the worker's message stays.
- Stored probes of PQ video without HDR10 mastering data are refreshed at job
  start (older versions didn't read it).
- A failure is logged once, at WARN, through its activity entry.
- Logging: the start line names the version and, when set, the build
  (`CHRYSOPOEIA_VERSION`); each hardware detection logs one INFO line
  ("hardware detection finished", with hwdetect's own summary at DEBUG);
  each finished job logs one plain INFO line, its activity entry ("Converted
  …", "Skipped …", "Cancelled …"; failures at WARN). Job starts, attempts
  and fallbacks are logged at DEBUG. Every log message stays on one line:
  line breaks in file names are written as escapes, so a file name can't
  forge a log line, and so is every other control character (an ESC byte
  can send commands to the terminal reading the log): the log formatter
  escapes every field, whatever the level (`file=…`, `path=…`), and the
  message.
- A converted original with other hard links, converted anyway, saves
  nothing: `files.saved_bytes`, the savings history and the activity line
  ("Converted <name>", with the job's note) count no saving.
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
- Explicit `-map 0:<index>` per kept stream. The primary video (no other
  video tracks, no data/timecode streams). Audio filtered by
  `audio_languages` (never drop all audio). Subtitles per
  `Container::subtitle_action` and `subtitle_languages`; attachments only for
  MKV.
- Cover images (`StreamInfo.is_attached_pic`: an MKV picture attachment such
  as `cover.jpg`, MP4 cover art) are kept where the container holds them.
  MP4 keeps JPEG, PNG and BMP covers as copied picture streams marked as the
  cover (`-c:v:N copy -disposition:v:N attached_pic`); every video option
  then names the video alone (`-c:v:0`, `-filter:v:0`, `-crf:v:0`,
  `-tag:v:0 hvc1`, …) and a decoder named for the input (`-c:v h264_qsv`)
  names the video's stream index (`-c:<index>`), or ffmpeg would filter the
  cover or read it as video. MKV keeps every cover as an attachment: ffmpeg's
  MKV writer would store a copied picture as a second video track, so each
  cover is copied out of the original first (`FfmpegPlan::covers`,
  `plan::cover_extract_args`: `-map 0:<index> -c copy -frames:v 1 -f rawvideo`,
  byte for byte) to `<work file stem>.cover-<n>.<ext>` next to the work file
  and attached (`-attach`, with `mimetype` and `filename` from the cover's
  own tags, `StreamInfo.filename`/`mimetype`, else from its codec and
  `cover.<ext>`). WebM holds no cover. Verification and the stream counts
  ignore cover images (they read back as attached pictures).
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

**run_job**: `JobSpec.mounts` are the mount points the library folder,
the work folder, the output folder (with the drives mounted inside it)
and the folder the new file goes into were seen on, each with what was
mounted there (`slow_fs::KnownMount`; see "Drives and shares the folders
sit on"). Before the job creates its temp file and before it starts
putting the new file in place, it reads `/proc/self/mountinfo` again
(`slow_fs::first_unmounted`): one that is no longer mounted, or has
something else mounted there (a share unmounted while the job ran, a
tmpfs or the bare folder mounted in its place), ends the job with
`NotResponding { path: <mount point> }`, nothing written, its temp file
removed, and the dispatcher's wait says the share isn't connected (or
that a different drive is mounted there).
1. Preparing: the input exists (answering within 30 s; otherwise the job
   ends with `JobOutcome::NotResponding { path }`, which the dispatcher
   turns into a requeue with the library offline), `decide` agrees
   (`decide_forced` with `force`), the destination is free (another
   running job of another file aiming at the same name is checked by the
   server first, see Dispatcher) and its folder
   writable (a new name longer than 255 bytes, or one that can't be checked,
   is refused here, not after the encode), and the temp folder has room.
   The room reserved is what the encode may grow to: 1.1× the original,
   1.5× without a size rule, 3× when the source codec is one step more
   efficient than the target (HEVC/VP9 → H.264, AV1 → HEVC) and 4× for two
   steps (AV1 → H.264), counting space promised to other running jobs (a
   job waits for room other jobs hold); a job alone on the disk still runs
   when 1.1× fits. The input's size+mtime (`FileIdentity`) is recorded.
   When originals are replaced (and not with `force`), two kinds of file
   are left as they are (`Skipped`): one with other hard links ("This file
   has another hard link (for example a torrent that is still seeding), so
   replacing it would use more space instead of saving it. It was left
   unchanged; Convert anyway converts it all the same"; converted anyway,
   the job notes "The original has another hard link (for example a seeding
   torrent), so replacing it freed no space"), and one whose target
   container can't hold everything it has (`plan::replace_loss`): wanted
   picture-based subtitles (MP4, WebM), wanted ASS/SSA subtitles that would
   become plain text (MP4, WebM: positions, colours and fonts are lost and
   overlapping lines cut short; ffprobe can't tell a styled track from a
   plain one, so every such track counts), attachments (only MKV holds
   them; fonts count only while subtitles are kept) and cover images (WebM;
   MP4 for covers other than JPEG, PNG or BMP). The reason always starts
   "{container} can't hold this file's {losses}, so it was left unchanged."
   (the web UI matches it), the losses being "N picture-based
   subtitle(s)", "N styled subtitle(s)", "N subtitle font(s)" (or "N
   attached file(s)" when not all are fonts) and "N cover image(s)", in that
   order, joined as "a, b and c": "MP4 can't hold this file's 2
   picture-based subtitles, 1 styled subtitle and 1 subtitle font, so it was
   left unchanged. To convert it, choose an MKV goal or save converted files
   to a separate folder; Convert anyway converts it without them". Cancel and
   Stop end the job at once even while these checks wait on a share.
2. Transcoding: for each candidate, an attempt with `hw_decode` as given; a
   failed hardware attempt is retried with CPU decoding, then the next
   candidate. Stop at the first success. Cover images a new MKV attaches
   are copied out of the original once, before the first attempt that
   needs them, and deleted when the job ends however it ends (crash
   recovery deletes leftovers: their names carry the job's `.tmp.` marker).
   A cover that can't be copied out fails the job (`other`, the original
   untouched): "The file's cover image couldn't be copied for the new file,
   so the file wasn't converted and the original was left unchanged. The
   job's log has the details." (a full disk or a folder that can't be
   written is described as for an encode). A failure that any encoder would
   hit the same way ends the job at once, without the "None of the N ways"
   prefix: a `disk_full`, `work_folder` or `destination` problem, or ffmpeg
   not starting at all. Cancellation kills ffmpeg at once
   (SIGKILL: the temp output is discarded anyway, and x265/SVT-AV1 ignore
   SIGTERM for seconds while they flush) and removes the temp file. A
   process that makes no progress for 10 min counts as hung and is killed
   the same way: progress is a progress block whose frame count, output
   time or bytes written moved, or a new stderr line. (ffmpeg 7, as in the
   Docker image, keeps printing identical blocks every half second while
   it is stuck; those don't count.)
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
   attempt; any other failure fails the job, unless the original changed
   while it was being checked (the checks read it again): that is the
   `Skipped` of step 5, not a failed check.
7. Finalizing (below).

stderr noise that means nothing (libnuma's `set_mempolicy: Operation not
permitted` from libx265 under Docker's seccomp profile) is kept out of error
reasons and log tails. With `low_priority` ffmpeg runs under `nice -n 10`;
the program is checked first (found on `PATH` or at its path, and
executable), and `nice` exiting 126/127 with its own message counts too, so
a missing ffmpeg is "not started" (`other`, "ffmpeg wasn't found at …")
either way, never an encoder error quoting `nice`. Every ffmpeg/ffprobe
child is started with `core::process::end_with_parent` (Linux
`PR_SET_PDEATHSIG` = SIGKILL), so no encode outlives a killed server.

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
  from the original"); a `visual` check that couldn't compare the pictures
  at all (no video track found, no picture readable) leads with "The new
  file couldn't be compared with the original." A similarity score stays in
  the check's detail (the report) as a percentage, like the UI shows it
  ("Matches the original at 4 points (99.8% similar on average)"), never in
  the error, and every error ends with what to do: "The original was kept.
  Try again, or choose lighter checks in Settings › Output." Track counts
  name each kind ("1 video track and 2 audio tracks").

**Putting the new file in place on a share that stops answering.** A
rename that has started can't be called back, and abandoning it half way
would leave the files out of step with the database, so `finalize` runs on
its own thread (`finalize::start_finalize`) and is never abandoned. It
checks a stop flag at its safe points: before anything, after the staging
copy (before the original is touched), right after the original was moved
aside (it is put back) and before the new file takes its name; stopped
there, it undoes what it did and fails with `finalize::Undone` (the
original is where it was). Once the new file has its final name it
finishes. The job watches the original and the destination folder while
it runs:

- The share stops answering: the job ends with `NotResponding` at once
  (its slot free, its library offline, the job back in the queue) and the
  step goes on, not stopped: when the share answers it finishes.
- Cancel, Stop or shutdown: the stop flag is set; the job waits up to 3 s
  for the next safe point, then ends (`Cancelled`) and leaves the step to
  go on.

The step then reports how it ended through `run::take_unfinished(job_id)`.
Meanwhile the dispatcher holds the job's file back (no job starts on it),
the feed says "The converted … was being put in place when its folder
stopped answering. It is finished when the folder answers again; until then
the original is kept safe.", and Cancel or Skip of the job (back in the
queue) asks the step to undo itself. When it ends: the new file took its
place → the job is recorded `done` whatever it was recorded as before (that
is what happened to the file), with a note ("Its folder stopped answering
while the new file was being put in place; that was finished when the
folder answered again", or, when it had been stopped, "It was stopped while
the new file was being put in place, but that had gone too far to undo, so
it was finished"); undone or failed (the original kept) → nothing more is
recorded, and a job back in the queue converts the file again.

Before the job is recorded as anything (back in the queue, cancelled), it
is marked in the database (`jobs.placing` = 1, with the new file's size):
the share may finish the rename after the server has stopped (a rename a
FUSE or NFS server already received is applied when it answers again),
and what the stop recorded then says nothing about the files. The mark is
cleared when the step ends here (recording `done` clears it), and the
search for leftovers never hands such a job's backup to `recover_artifact`
while it is marked. After a restart the disk settles it
(`dispatcher::settle`), once per job at a time, before anything else is
done with the file or the backup: at start-up for every marked job whose
folder answers within 10 s; otherwise by the job itself when it runs again
(it settles every marked job of its file first: a folder that doesn't
answer puts its library offline as usual), every 15 s in the background
for a job that can't run again (cancelled), and by a search for leftovers
that finds its files. Replacements go by the backup
(`finalize::resume_replace`, which only looks): the backup next to a file
under the final name (the original's own name, or the new name while the
original's name is free) → in place; this is noted first (`placing` = 2,
with both sizes), then the backup is removed (`finalize::remove_backup`)
and the job recorded `done` with the note "Chrysopoeia stopped just as the
new file was being put in place. The new file was already complete, so it
was kept", its savings counted. (Noting it first means a removal that
finishes after a share answered late can't make the job look as if its
new file never got there.) A step that finished and removed the
backup just as the server stopped (its result no longer recorded) leaves
the new file itself to tell, when the mark has its size: no backup left
and a file of exactly that size where the new file goes (under the
original's name only when that isn't the original's size; under a new
name only while the original's name is free) → in place. Otherwise no
backup, or a backup without the new file → not in place: the original is
put back, the job's temp files go, and a job in the queue converts the
file again. Folder mode, where the original is
never touched, goes by the new file in the output folder with the size it
had and no other finished job claiming it. Two marked jobs aiming at one
name can't be told apart: neither is taken as done, and both originals go
back. A job in the queue whose earlier run wasn't marked (an older
version) is looked at the same way when it runs, by the backup its
earlier run would have left.

Only an answer settles a job, since a mark cleared without its backup
put back lets the next search for leftovers put that backup back next to a
new file that is in place (the job then fails on the name, or, for the
same name, is skipped as already converted and its record lost). "Not
found" is an answer; every other look gives none and leaves the job
marked with nothing touched (`Settled::Unreachable`, looked at again as
for a folder that doesn't answer): an error other than "not found" (a
soft-mounted share that timed out answers "read or write error", a FUSE
share whose server stopped "not connected"; `finalize::Unreadable`), a
folder whose listing fails, a leftover that couldn't be put back or
removed, and a database that couldn't be read. Nothing at all is looked
at, nor touched, while the drives and shares the job's files are on
aren't mounted as they were: the library folder's, in folder mode the
output folder's (and those mounted inside it), and the mount the folder
its new file goes into was on when the job started (`jobs.final_mount`:
a share mounted inside the output folder, or reached through a link). An
unmounted share leaves an empty folder, or one with files of its own,
where nothing is found, and another drive mounted in its place holds
neither the new file nor the backup (see "Drives and shares the folders
sit on"); the job waits, "not connected" or "A different drive is
mounted at …". "Not in place" also needs
the job's folders to be there as they should be: the library folder
answers, can be read and isn't empty, no
drive or share known to be mounted in the library above the file is
disconnected, and in folder mode the output folder answers and can be
read. A job that settles itself
then waits with its library offline, its reason the library's own
problem or "Chrysopoeia can't read the folder … because the disk reported
a read or write error. If it's on a drive or network share, check that
it's connected."; the check every 15 s keeps it so while the share
answers with errors. While a job is marked, its destination counts
as taken (another file's job aiming at it fails before encoding, as for a
running one), a scan never removes its file from the list, and history
trimming keeps it.

**finalize**: the verified temp file is first staged under a hidden name in the
destination folder (a rename, or across filesystems a copy that is flushed to
disk), so the original stays until the new file is completely there. The
original's identity is checked again right before it is touched. Replace mode
(same path or new extension): original → hidden backup
(`paths::backup_file_name`), staged → final, backup deleted; the backup is
checked to be the file the job read, a new name must still be free when the
new file moves in, and any failure puts the backup back. (A backup name longer
than 255 bytes: the new file is renamed in first, then a renamed original is
deleted.) A new name (folder mode, or a new extension) is claimed in the
process for the whole step, so two jobs aiming at one name can't both find
it free: the second fails with the usual "appeared in the output folder"
/ "appeared next to the original" message, and the first's file is never
overwritten. Folder mode: create folders, never overwrite, never touch the
original. The new file gets the original's permission bits, its owner and
group where allowed (the group alone when only that is), and, with
`keep_file_dates`, its times; a new file that couldn't keep the owner or
group gets a job note ("The new file couldn't keep the original's group
(group 1001), so it is in group 100. If your media server can't open it,
run Chrysopoeia as the owner of your media (PUID and PGID)"). MP4 and WebM
can't hold attachments: the plan notes the fonts it leaves out ("Left out 2
subtitle fonts because MP4 can't hold attachments"; "Left out 1 attached
file because …" when not all are fonts). Every loss `replace_loss` counts
gets a job note when the file is converted anyway or into an output folder:
"Removed 2 picture-based subtitles because MP4 can't hold them", "Converted
1 styled subtitle to plain text because MP4 can't keep its styling", the
attachment notes above, and "Left out 1 cover image because WebM can't hold
it" ("Left out 2 cover images because WebM can't hold them"). After a crash,
`resume_replace(input, final_path, job_id)` reports `Placed` when the job's
backup exists and the new file is in place — the original's name taken
again (same path) or free with the new name present (new extension) — else
`NotPlaced` (only when every look answered: an error other than "not
found" is a `finalize::Unreadable`, never `NotPlaced`); it only looks (it
may be asked again), and
`remove_backup(input, job_id)` then removes the backup. `recover_artifact`: temp and staged files
are deleted; a backup is renamed back when the original is missing, deleted
otherwise. A failure to put the new file in place is a
`finalize::PlaceError` (inside the `anyhow::Error`) with its message and
problem kind.

## Problems (normative)

Every failed job carries `JobOutcome::Failed.problem`, a `ProblemKind`
recorded as `Job.problem` and, while the file is failed, `MediaFile.problem`
(both also in `job.updated` / `file.updated` events). The UI groups problems
and picks the fix by this code, never by reading sentences.

| Kind | When | Examples |
|---|---|---|
| `unreadable_source` | The original can't be read as a video, or stops early | ffprobe rejects it at scan time; a cut-off download ("The original file appears damaged or incomplete (it stops after 0.1 s)…"); no readable audio track; no read permission |
| `work_folder` | The work folder (`settings.temp_dir` / `TEMP_DIR`) can't be used | a file in the way of its name; no write permission; a read-only drive |
| `destination` | The new file can't be put where it belongs | a read-only library or no write permission (checked before encoding); a file already using the new name (another library's converted file with the same relative path in the output folder); a new name too long for the disk; folder mode without an output folder |
| `disk_full` | Not enough room | the work folder can't hold ~1.1× the original even with no other job running; the disk filled up while encoding or copying (with other jobs running, the job is first tried again once) |
| `encoder` | The encoder failed on every attempt | ffmpeg stops with an error, stops making progress for 10 min, or writes nothing |
| `hardware_unavailable` | The hardware chosen in Settings can't make the codec and CPU fallback is off | also a worker given no encoder at all |
| `verification` | The new file failed its checks on the last attempt | "The new file is shorter than the original …" |
| `source_changed` | The original disappeared before or during the job (an original that was changed or replaced meanwhile is a skip, not a failure) | moved or deleted while queued or converting; gone right before replacing |
| `other` | Anything else | the converter crashed, a database problem, ffmpeg not installed |

A `done` file that is converted again keeps `done` and no problem whatever
the new job's outcome, unless the file itself is gone (see Database rules);
only the job records it.

**Wording.** Every user-facing message (job and file errors, notes, the
activity feed, setup hints, API errors) is one or two plain sentences: what
happened, then what to do ("The work folder /temp can't be used because a
file with that name is in the way. Fix it, or choose another work folder, in
Settings › Output."). Messages never carry raw OS errors or error numbers
(`chrysopoeia_core::plain::io_reason` turns an `io::Error` into a few words
such as "the disk is full"), encoder names ("Converting on the NVIDIA GPU
didn't work…", not `hevc_nvenc`), exit codes, or paths without saying what
the folder is ("the work folder /temp", "the output folder /out/TV", "the
original's folder /media/Films"); the fix fits the output mode (in folder
mode: choose another output folder; replacing: make the drive writable or
save to a separate folder). A copy refused for lack of room gives the new
file's size, not the margin kept free. A size reads the same wherever it
appears (the activity feed, a job's check lines, the web UI): decimal units,
whole KB, then two decimals below 10, one below 100 and none above, rounded
before moving up a unit ("3.13 MB", "572 KB", "1 GB"); one formatter,
`chrysopoeia_core::format::bytes`, follows the web's `formatBytes`.
An unfamiliar ffmpeg failure is described plainly and then quoted
("Converting on the CPU stopped with an error, so the original was left
unchanged. ffmpeg said: "…""); the job's `log_tail` keeps the details.

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
| `GET /overview` | | `Overview` (`totals` and `savings_history` cover the same libraries; `resolutions` has a "No video" bucket for files without a video stream, "Unknown" for pictures whose size couldn't be read) |
| `GET /libraries` | | `Library[]` (`path_error` set when the folder is missing, unreadable or offline, or on a drive or share seen mounted there that isn't now: "The drive or share mounted at … isn't connected. …", or has another drive in its place: "A different drive is mounted at … than before. …", with `changed_mount` the place, also when that is the output or work folder's or where an unsettled conversion's new file goes) |
| `POST /libraries` | `{"path", "name"?, "profile"?, "goal"?}` | `Library` (201). 400 `path_required`/`path_not_absolute`/`path_not_found`/`not_a_directory`/`not_readable`/`path_not_supported`/`folder_not_allowed`/`contains_output_folder`/`invalid_name`/`invalid_profile`, 409 `library_exists`/`library_overlaps`. `folder_not_allowed`: the folder (as the disk has it, links followed) is `/`, the data folder or inside it or above it, `/config`, `/app` or the web folder or inside them, or `/proc`, `/sys` or `/dev` or inside them; the `LIBRARIES` start-up list follows the same rule. Starts a scan. |
| `GET /libraries/{id}` | | `Library` |
| `PATCH /libraries/{id}` | `{"name"?, "enabled"?, "profile"?}` (profile is normalized; response includes it) | `Library`. Profile changes re-decide `pending`/`skipped` files (not done/failed; files skipped by the user stay skipped); a file being converted is decided again when its job ends (see "Verdicts and goal changes"). |
| `DELETE /libraries/{id}` | | 204. Removes DB rows only (files, jobs and its share of the savings history), never media. Cancels its running jobs. |
| `POST /libraries/{id}/scan` | | 202 `{"started":true}` (409 `scan_running`, 409 `library_disabled`) |
| `POST /libraries/{id}/relearn-mounts` | | `Library`. Takes the drive mounted now as the usual one at every place, among those the library's folder, the work folder and the output folder sit on and those its unsettled conversions' new files go into, where something else is mounted than before; places with nothing mounted stay "not connected". Logs "<library> now uses the drive mounted at …", and checks the waiting libraries again. 409 `nothing_changed` when no such place has another drive. |
| `POST /scan` | | 202, scans all enabled libraries |
| `GET /files` | `status`, `library`, `q` (substring of the name or path, or of a name the file had before a conversion renamed it), `sort` (`name`,`size`,`updated`,`status`; prefix `-` for desc), `limit` (≤500, default 100), `offset` | `{"items": MediaFile[], "total"}` (no `probe`; `problem` with every `error`) |
| `GET /files/{id}` | | `{"file": MediaFile (with probe), "jobs": Job[] (newest first, ≤10)}` |
| `POST /files/{id}/queue` | `{"priority"?: int, "force"?: bool}` | `Job` (`force` echoed). Works for pending/failed/skipped/done (re-encode). `force` = "Convert anyway" (see `decide_forced`, no size rule; verified as usual). A done file whose new job ends without a new result (skipped, cancelled, failed) stays done. 409 `already_queued`. |
| `POST /files/{id}/skip` | | `MediaFile` status skipped, reason "Skipped by you"; cancels its job |
| `POST /files/bulk` | `{"action":"queue"\|"skip"\|"retry_failed", "ids"?: [], "library"?, "status"?}` | `{"affected": n, "left_out": m}`. `queue` with `ids` only queues failed files and files the library's goal would convert (`decide`), except files skipped by the size rule under that goal; the rest are counted in `left_out` (0 for other selections). |
| `GET /jobs` | `state` (`active` = running+queued, `running`, `queued`, `history` = finished, `all`), `limit`, `offset` | `{"items": Job[], "total"}`; active sorted running-first then queue order; history newest first |
| `GET /jobs/{id}` | | `Job` (includes `notes: string[]`, `validation`, `command`, `log_tail`, `problem`, `force`, `freed_bytes`, `output_name`) |
| `POST /jobs/{id}/cancel` | | `Job` (409 `job_finished`) |
| `POST /jobs/{id}/priority` | `{"priority": int}` or `{"move":"top"}` | `Job` |
| `POST /jobs/clear` | `{"state":"history"}` | `{"affected": n}` deletes finished job rows (files keep status) |
| `GET /queue` | | `QueueState` |
| `POST /queue/pause` / `POST /queue/resume` | | `QueueState` |
| `POST /queue/stop` | | `QueueState` (cancel running, re-queue them, pause) |
| `GET /settings` | | `Settings` |
| `PATCH /settings` | partial `Settings` JSON (merged at top level; `default_profile` replaced whole) | `Settings`. 400 `invalid_settings`/`unknown_setting` with `field` (e.g. folder mode without folder, unwritable temp dir, an added ignore pattern that is invalid; patterns already saved don't block other changes). An output or work folder saved again as it was (or through a link to the same place) keeps the drives remembered for it, and one whose drive isn't connected as it was isn't written into to check it; only a folder that changes is learned afresh. |
| `GET /settings/folders` | | `FolderStatus[]` `{setting: "output_folder"\|"temp_dir", path, problem, changed_mount}`: the output folder (folder mode) and the work folder in use (`temp_dir`, else the one the server was started with), `problem` the same sentence as a library's `path_error` when its drive isn't connected as it was, `changed_mount` the place when another drive is mounted there. No disk is touched. |
| `POST /settings/relearn-mounts` | | `FolderStatus[]`. Takes the drive mounted now as the usual one where the output or work folder's drive was and something else is mounted (also for the unsettled conversions whose new file goes there); places with nothing mounted stay "not connected". Logs "The output folder … now uses the drive mounted at …", and checks the waiting libraries again. 409 `nothing_changed` when no such place has another drive. |
| `GET /hardware` | | `HardwareInfo` (`detecting: true` placeholder until the first detection ends) |
| `POST /hardware/detect` | | `HardwareInfo` (re-runs detection, ~seconds) |
| `GET /presets` | | `{"goals": [{"goal","title","summary","profile"}]` (`summary`: a plain one-line outcome, no codec names or speed claims; kept for compatibility, the UI has its own copy), "video_codecs": [{"codec","label","royalty_free","hw_accelerated", "encoders": [verified names]}], "audio_codecs": [{"codec","label"}], "containers": [{"container","label","video": [...], "audio": [...]}]}` — only codecs with a verified encoder (a listed CPU encoder when detection failed) and audio codecs whose encoder ffmpeg has (plus `copy`); everything while detection runs |
| `GET /fs/browse` | `path` (default: first browse root) | `{"path","parent": string\|null,"roots": [string],"media_count"?: n,"media_count_capped"?: bool,"library_blocked"?: string,"entries":[{"name","path","is_dir":true,"media_count"?: n,"media_count_capped"?: bool}]}` directories only, sorted, hidden dirs and links out of the roots excluded. `media_count`: video files (not audio-only ones) in the folder and up to 4 levels below, hidden entries skipped, links to folders not followed; counting stops after 2 000 entries or ~150 ms per folder (`media_count_capped: true`, "at least n"), and after ~2 s per listing, or 300 folders, later folders get no count. The top-level pair is the browsed folder itself, so the picker can say what choosing it brings: its own video files plus the entries' counts (linked folders left out, as a scan doesn't follow them), capped when any entry is capped or has no count, so it is never lower than a subfolder's; left out when a file can't be checked. `library_blocked`: why this folder can't be a library (the `folder_not_allowed` sentence), left out when it can; the picker shows it and disables "Use" while a library's folder is chosen, and stays free for the output and work folders. 400 `path_not_absolute`/`not_a_directory`/`not_readable`, 403 `outside_roots`, 404 `path_not_found`/`no_browse_roots`, 503 `not_responding` (the folder didn't answer within 10 s, or its share already has as many stuck checks as it may) / `busy` (too many checks of other shares are stuck: try again) |
| `GET /activity` | `limit` (≤500, default 100), `before` (id) | `{"items": ActivityEntry[]}` newest first (`problem` on entries about a failed file) |
| `GET /ws` | WebSocket | `Event` JSON messages (see `core::event`) |

Other codes: 404 `not_found` (unknown path), `library_not_found`,
`file_not_found`, `job_not_found`; 405 `method_not_allowed`; 400
`invalid_json` ("The request body isn't valid JSON. It may be cut off, or
have a missing quote, bracket or comma."), `invalid_request` (with `field`
when serde names one: "The value for "path" isn't valid. It must be text.";
without one: "The request is missing "path"." or "The request body should be
an object with named values, like {"name": "value"}." — never the parser's
text or an internal type name; bodies that may be left out, such as
`POST /files/{id}/queue`, answer the same way), `invalid_query` (plain words naming the parameter, with `field`: "The value
of "offset" isn't valid. It must be a whole number, 0 or more."),
`invalid_path_param` (""not-a-uuid" isn't a valid id. …", never the
parser's text), `invalid_status`, `invalid_sort`, `invalid_state` ("Filter
jobs by all, active, running, queued or history."), `invalid_library`; 403 `host_not_allowed`,
`forbidden_origin`; 409 `job_finished`; 413 `body_too_large`; 415
`unsupported_media_type`. An ignore pattern that can't be used is refused
with a plain reason naming it (`The ignore pattern "Movies/[abc" has a [
without a closing ].`), never the glob library's wording.

WebSocket: on connect the server sends `queue.state` and `stats.updated`
immediately. Server pings every 30 s. A lagging client just misses events;
clients refetch on reconnect.

**Request guard** (no accounts; trusted LAN): the `Host` header must be an
IP literal, `localhost`, a single-label name, a name under `.local`, `.lan`,
`.home`, `.home.arpa`, `.internal`, `.localdomain`, `.localhost`, `.ts.net`,
`.fritz.box`, or listed in `ALLOWED_HOSTS`; else 403 `host_not_allowed` (DNS
rebinding: a browser always sends the name it looked up as `Host`).
`X-Forwarded-Host` is not held to this list (a page can't set it without a
CORS preflight, which is never granted); it only serves the origin check
below, for proxies that pass the upstream address as `Host`. Requests that
change something and WebSocket upgrades carrying an `Origin`/`Referer` must
come from the same host as `Host` or `X-Forwarded-Host`; else 403
`forbidden_origin`. A port left out means the default port of the origin's
scheme. When `Host` (or `X-Forwarded-Host`) has no port and the request
came through a reverse proxy (it carries `X-Forwarded-For`,
`X-Forwarded-Host`, `X-Forwarded-Proto`, `Forwarded` or `X-Real-IP`), as
Nginx Proxy Manager and others send it, only the host names are compared
(`Origin: https://name:8443` matches `Host: name`); without a proxy, a page
on another port of the same host is refused. The host allowlist still keeps
other websites out. Browsers mark requests with `Sec-Fetch-Site`: `/api`
requests marked `cross-site` are refused (reads too, e.g. a hidden image
that makes the folder picker spin disks up), and `same-site` ones (a page
on another port of the same host) must pass the origin check, so one
without `Origin`/`Referer` is refused. Both refusals end with "If you use a
reverse proxy, make it pass the original Host header." Requests without
`Origin`, `Referer` and `Sec-Fetch-Site` (curl, scripts) pass. No CORS
headers are sent (except with `--dev-cors`, which also skips the
`Sec-Fetch-Site` check).

### Contract additions (round 5)

- **`Job.freed_bytes`** (`integer | null`, `jobs.freed_bytes`): the disk
  space the conversion actually released. For a done job it is the input's
  size minus the output's in the usual case, and `0` when the original's
  space was not released (the original had another hard link, so replacing
  it freed nothing: the same rule as `MediaFile.saved_bytes`, taken from
  the worker's "…so replacing it freed no space" note) or the output is
  not smaller (never negative). `null` for a job that is not done and for
  one that finished before the field existed. Written by `db::jobs::finish`
  in the transaction that marks the job done.
- **`ActivityEntry.problem`** (`ProblemKind | null`, `activity.problem`): set
  for an entry written about a failed or skipped file whose problem kind is
  known, with the same values as `Job.problem`: the problem of the failed
  job the entry refers to, else (an entry about a file without a job) of
  the file when it is failed. `null` for success entries, entries about a
  library as a whole, and everything else. It is the kind the failure had
  when the entry was written, and goes out in the `activity` event too.
- **File search** (`q` of `GET /files`) also matches a name the file had
  before a conversion renamed it (`q=land1080.mp4` finds the file now named
  `land1080.mkv`), by the names recorded on the file's jobs
  (`jobs.file_name`, the name the file had when the job was queued). Still a
  case-insensitive substring, and a file is listed once however many jobs
  match.
- **`Job.output_name`** (`string | null`): the file name of the result when
  it differs from the original's (the extension changed, say), for a done
  job; `null` otherwise. Derived from `jobs.final_path` (where the job put
  its result, stored when it started), so nothing more is stored.

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
  changes. It refers to another hint only when one is about that hardware
  (its maker, or passing /dev/dri in for Quick Sync, VA-API and AMF);
  otherwise it gives the fix itself (NVIDIA: the Nvidia-Driver plugin and
  `--runtime=nvidia` settings; Intel/AMD: `--device=/dev/dri`; or choose
  Automatic). A busy GPU gets a warning saying files wait (or use the CPU)
  until it is free and that Chrysopoeia checks again by itself.
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
(`NEXT_PUBLIC_API_URL` overrides for `next dev`) and stays current through
`/api/ws` events. No mock data in the bundle. State lives in the URL hash
(`#/queue/history`), so reloads and links keep the view.

Navigation: a sidebar on wide screens (Overview, Queue, each library, "Add
library", Settings; appearance switch), a bottom tab bar on phones
(Overview, Queue, Libraries, Settings). The queue link shows what is
running. Details of a job or file open in a sheet over the current screen.

Screens:
1. **Setup** (first run, when `settings.onboarded` is false and there are no
   libraries): welcome (what Chrysopoeia does, three reassurances) → pick a
   folder with the server-side folder picker, which shows how many videos
   each folder and the current one hold (`media_count`) → choose a goal
   (Save space / Balanced / Plays everywhere / Archive, each with its
   trade-off and the detected hardware's speed) → start (creates the
   library, scanning begins, `onboarded = true`). Later libraries are added
   with the same two steps under "Add library".
2. **Overview**, a status page: the space saved as one big number with a
   one-line status sentence (what is happening now, what is left) and a
   savings chart once there is history worth charting; **Needs your
   attention** (setup hints from hardware detection, libraries that can't be
   reached, failed files grouped by `problem` with the fix for each);
   **Converting now** (live cards per running job: file, stage, progress,
   speed, time left, where it runs); **Libraries** (each library's
   progress, and "Add library"); the latest finished results. No codec
   breakdown and no activity feed. Without libraries it shows the welcome
   and "Choose a folder".
3. **Queue**: tabs **Running** / **Up next** / **History** with counts.
   Running jobs can be stopped; in Up next a job can be moved to the top or
   taken out of the queue; History lists finished jobs (converted, kept as
   they were, failed, cancelled) with "Try again" and "Convert anyway" where
   they apply, and can be cleared. The activity **Log** (scans, warnings
   and problems) sits under History. Queue controls: pause, resume, stop
   now. A job's sheet
   shows before → after, notes, the verification report, and the ffmpeg
   command and log tail under a disclosure.
4. **Library** (one per library, tabs **Files** and **Settings**): Files is
   the library's progress and the file table with search, status filters
   (with counts), sort, pages and bulk Convert / Skip, plus the
   files-still-copying count (`LibraryStats.settling`) and the folder's
   `path_error` (in a callout at the top; when that is another drive
   mounted in place of the usual one, `changed_mount`, the callout is
   "A different drive is mounted" with **Use the drive that's there now**,
   which asks first: "Chrysopoeia will take the drive mounted at … as the
   usual one from now on: it reads files from it and saves new files to
   it. Only do this if you replaced the drive or share on purpose. If the
   usual one just isn't connected yet, reconnect it instead: files saved
   now would end up on the drive that's there now."); Settings holds the library's goal, quality, speed and
   advanced format choices, its name, and pausing or removing the library
   (media is never touched). "Scan now" is in the library's menu.
5. **Settings**, in sections: **Processing** (Files at once: Automatic (n,
   and its source) or a number; when to convert: active hours; finding
   files: watch folders, rescan interval, auto-queue); **Output** (finished
   files: replace the original or save to a separate folder; the work
   folder, naming `SystemInfo.default_temp_dir` as the automatic one; keep
   file dates; **Checks before replacing**: the verification level, with a
   warning when checks are off); **This machine** (detected processor,
   memory and graphics, setup tips, hardware preference and CPU fallback,
   "check again", and encoders and ffmpeg details under a disclosure);
   **Advanced** (ignored files, minimum file size in MB, defaults for new
   libraries); and **About** (version, build, and a copyable bug-report
   summary). Changes are saved with one save bar; a field error is shown
   next to the field named by the API's `field` and in the save bar. The
   output or work folder picked again as it is is no change. When the
   drive the saved output or work folder sits on isn't connected as it
   was (`GET /api/settings/folders`, looked at again every 15 s), its
   block says so in a callout with the server's sentence ("This folder's
   drive isn't connected", or "A different drive is mounted" with **Use
   the drive that's there now** and the same confirmation as a library's,
   which calls `POST /api/settings/relearn-mounts`); hidden while another
   folder is picked.

Plain language first: "Smaller files", not "CRF 32"; codecs are secondary
detail; problems are grouped by `problem` and each comes with its fix;
every destructive action is reversible or confirmed.

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
