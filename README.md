# Chrysopoeia

Chrysopoeia converts your whole media library to smaller or more compatible
video in the background: point it at a folder, pick a goal, and it handles the
rest. Every new file is decoded end to end and visually compared with the
original before it is allowed to replace it.

It runs as one Docker container with one web page, on Unraid or any Linux,
Windows or macOS machine that runs Docker (or natively on a Mac).

## Why Chrysopoeia

Tdarr is a powerful, flexible tool built around plugin stacks and separate
worker nodes. Chrysopoeia is deliberately narrower, and aims to be as robust
while asking far less of you:

- **Goals instead of plugins.** Choose *Save space*, *Balanced*, *Plays
  everywhere* or *Archive*. Codec, container, quality and audio handling follow
  from that; every setting can still be changed under Advanced.
- **One container, nothing to wire up.** The server, the job queue, the workers
  and the web UI are a single process on port 8080. No nodes, no separate
  database.
- **Hardware set up for you.** On start it finds your GPUs, runs a one-second
  test encode on each hardware encoder, and only uses the ones that actually
  work. The number of jobs to run at once is worked out from your CPU cores,
  memory, container limits and GPU.
- **Verified before replaced.** Each result must decode cleanly from start to
  finish, keep the expected streams and duration, and match the original
  visually (SSIM at several points in the file). Only then is the original
  swapped out, in a way that survives crashes and power loss.
- **Plain explanations.** Every skipped or failed file says why, in a sentence.
  GPU setup problems are shown in the app with the exact setting to change.

## Features

- Inputs: anything ffmpeg can read (MKV, MP4, AVI, MPEG-TS, MOV, WMV, ...).
- Outputs: AV1, HEVC (H.265), H.264 or VP9 video in MKV, MP4 or WebM; audio kept
  as-is or converted to Opus, AAC, FLAC, AC-3, E-AC-3, MP3 or Vorbis.
- Hardware encoding: NVIDIA NVENC, Intel Quick Sync, VA-API (Intel and AMD),
  Apple VideoToolbox, plus CPU encoders (SVT-AV1, x265, x264, libvpx). If a
  hardware encode fails, the file is retried on the CPU automatically.
- Keeps subtitles, chapters, metadata, HDR signalling and 10-bit colour where the
  target allows; deinterlaces broadcast recordings; skips files that are already
  efficient.
- Watches folders and picks up new files, with an optional schedule (active
  hours), pause, priorities and retry.
- Replaces originals in place, or writes to a separate output folder and leaves
  originals alone.

### Goals

| Goal | Video | Audio | Container | Keeps the result only if |
|---|---|---|---|---|
| Save space | AV1 | Opus | MKV | at least 10% smaller |
| Balanced | HEVC | original | MKV | at least 10% smaller |
| Plays everywhere | H.264 | AAC | MP4 | always (compatibility is the point) |
| Archive | AV1, highest quality | original | MKV | at least 5% smaller |

## Quick start

### Unraid

1. For hardware encoding, first install the driver plugin from Community
   Applications: **Intel GPU TOP** (Intel), **Radeon TOP** (AMD) or
   **Nvidia-Driver** (NVIDIA). CPU-only works without any plugin.
2. Install **Chrysopoeia** from Community Applications. Until it is listed
   there, add the template by hand from the Unraid terminal:

   ```sh
   wget -O /boot/config/plugins/dockerMan/templates-user/my-Chrysopoeia.xml \
     https://raw.githubusercontent.com/thekozugroup/Project-Chrysopoeia/main/unraid/chrysopoeia.xml
   ```

   then open **Docker > Add Container** and pick *Chrysopoeia* from the
   Template list.
3. Set **Media** (required) to the share that holds your videos, for
   example `/mnt/user/media/`, rather than all of `/mnt/user/`. Intel or AMD:
   click *Add another Path, Port, Variable, Label or Device*, choose *Device*
   and enter `/dev/dri`. NVIDIA: add `--runtime=nvidia` to *Extra Parameters*
   and set `NVIDIA_VISIBLE_DEVICES` (under *Show more settings*) to `all`.
4. Click **Apply**, then open the web UI from the container's icon.

The full walkthrough, including a transcode cache on your SSD and
troubleshooting, is in [docs/UNRAID.md](docs/UNRAID.md).

### docker run

```sh
docker run -d --name chrysopoeia --restart unless-stopped \
  -p 8080:8080 \
  -v /srv/chrysopoeia:/config \
  -v /srv/media:/media \
  -e PUID=1000 -e PGID=1000 -e TZ=Europe/London \
  ghcr.io/thekozugroup/chrysopoeia:latest
```

Add a GPU with one extra flag:

```sh
# Intel or AMD
--device /dev/dri:/dev/dri
# NVIDIA (needs the NVIDIA Container Toolkit on the host)
--gpus all                                    # one card: --gpus device=GPU-<uuid>
# NVIDIA through the runtime instead (what Unraid uses)
--runtime=nvidia -e NVIDIA_VISIBLE_DEVICES=all   # one card: its UUID instead of all
```

Then open `http://<server>:8080`.

### Docker Compose

```sh
curl -O https://raw.githubusercontent.com/thekozugroup/Project-Chrysopoeia/main/docker-compose.yml
curl -o .env https://raw.githubusercontent.com/thekozugroup/Project-Chrysopoeia/main/.env.example
# edit .env: MEDIA_PATH, PUID, PGID, TZ
docker compose up -d
```

With a GPU, add the matching overlay file from this repository:

```sh
docker compose -f docker-compose.yml -f docker-compose.nvidia.yml up -d      # NVIDIA
docker compose -f docker-compose.yml -f docker-compose.intel-amd.yml up -d   # Intel / AMD
```

## GPU setup

Chrysopoeia uses whichever encoders pass its test encode, so the only job is
making the GPU visible to the container. The **Hardware** section of Settings
shows what was found and, if something is missing, the fix.

| Hardware | Host needs | Container needs | Encodes in hardware |
|---|---|---|---|
| NVIDIA GeForce / RTX / Quadro | NVIDIA driver + NVIDIA Container Toolkit (Unraid: Nvidia-Driver plugin) | `--gpus all`, or `--runtime=nvidia` and `NVIDIA_VISIBLE_DEVICES=all` | H.264 (Kepler+), HEVC (GTX 950 and newer), AV1 (RTX 40 and newer); the GT 1030 and most MX laptop chips have no encoder |
| Intel iGPU or Arc | `i915` or `xe` driver loaded, `/dev/dri` present (Unraid: Intel GPU TOP plugin) | `--device /dev/dri` (Unraid: a Device `/dev/dri`) | H.264, HEVC (6th gen+), AV1 (Arc, Core Ultra) |
| AMD Radeon / Ryzen APU | `amdgpu` driver, `/dev/dri` present (Unraid: Radeon TOP plugin) | `--device /dev/dri` (Unraid: a Device `/dev/dri`) | H.264, HEVC (RX 400 and newer), AV1 (RX 7000 and newer) |
| Apple Silicon Mac | nothing: run Chrysopoeia natively, not in Docker (Docker on macOS cannot reach the GPU) | n/a | H.264, HEVC |
| Raspberry Pi 4 | 64-bit OS | `--device /dev/video11` (encoder) | H.264 only; the Pi 5 has no hardware encoder |

No GPU at all is fine: CPU encoding is slower but gives the smallest files. The
detailed support matrix, including older GPUs and ARM boards, is in
[docs/HARDWARE.md](docs/HARDWARE.md).

## Configuration

Almost everything is set in the web UI. The container reads these environment
variables:

| Variable | Default | What it does |
|---|---|---|
| `PUID` / `PGID` | `1000` / `1000` | User and group Chrysopoeia runs as and writes files as. Use the owner of your media (Unraid: `99` / `100`). |
| `UMASK` | `002` | Permissions for new files (`002`: group can edit). |
| `TZ` | `UTC` | Time zone for logs and the active-hours schedule (Unraid sets it for you). |
| `HW_ACCEL` | `auto` | Hardware preference on first start, changeable later in Settings: `auto`, `cpu`, `nvenc` (NVIDIA), `qsv` (Intel), `vaapi` (Intel or AMD), `amf` (AMD's proprietary driver, not in the image), `rkmpp` (Rockchip), `v4l2m2m` (Raspberry Pi 4) or `videotoolbox` (native macOS only). |
| `MAX_JOBS` | automatic | Files converted at once, 1 to 32. Leave unset for automatic; Settings overrides it. |
| `LIBRARIES` | none | Comma-separated folders (container paths) to add as libraries on first start, e.g. `/media/Movies,/media/TV`. |
| `TEMP_DIR` | `/temp` if mounted | Where in-progress files go. Unset and no `/temp` mount: next to each original. |
| `BROWSE_ROOTS` | `/media,/` if `/media` is mounted | Folders the in-app folder picker starts from. |
| `NVIDIA_VISIBLE_DEVICES` | unset | NVIDIA with `--runtime=nvidia` (Unraid): `all` or a GPU UUID. With `--gpus` or the compose overlay, Docker sets it from the GPUs chosen there. |
| `NVIDIA_DRIVER_CAPABILITIES` | `compute,video,utility` | Already set in the image; needed for NVENC. |
| `PORT` | `8080` | Port inside the container. |
| `LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug` or `trace`. |

| Path | Purpose |
|---|---|
| `/config` | Database and settings. Small; back it up. |
| `/media` | Your media. Needs write access so originals can be replaced. |
| `/temp` | Optional scratch space on a fast disk (SSD or cache pool). |

## FAQ

**Are my originals safe?**
Chrysopoeia never writes over a file it has not verified. Each conversion goes
to a hidden temporary file; after it passes verification, the original is
renamed to a hidden backup, the new file is moved into place, and only then is
the backup deleted. If the power fails or the container stops halfway, the
next start finds the backup and puts it back. A failed or cancelled job leaves
the original untouched. If you would rather keep originals, choose *Output
folder* in Settings and Chrysopoeia will never modify your library. As with any
tool that rewrites files, keep a backup of media you cannot replace.

**What does "Verified" mean?**
The new file was opened and checked before it replaced the original. With the
default *Standard* level that means: it has the expected video, audio and
subtitle streams in the target codec; its duration matches the original; it
decodes from start to finish without a single error; and at four points in the
file its picture was compared with the original (SSIM, a standard measure of
visual similarity), catching green frames, blocking and other corruption.
*Thorough* samples ten points and also checks for added black or frozen
frames; *Quick* only checks streams and duration. Each job's report is in the
Queue.

**Why was a file skipped?**
The reason is shown next to the file. The usual ones: the video is already as
efficient as the target (for example it is already AV1); it is audio-only; it
could not be read or is shorter than a second; the converted file was not
enough smaller to be worth keeping (the original is kept); it is HDR and the
goal is H.264 (tone mapping is not supported yet); or you skipped it.

**How many jobs run at once?**
By default it is automatic. On the CPU: one job per four cores (1 to 8),
limited so each job has about 1.5 GB of memory, and respecting any CPU or
memory limit set on the container. With a GPU: 3 per NVIDIA GPU, 2 per Intel or
AMD GPU, 2 on Apple Silicon. You can set a fixed number in Settings >
Processing, or with `MAX_JOBS`.

**Is there a login?**
Not yet. Keep Chrysopoeia on your home network, or put it behind a reverse
proxy that adds authentication.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md). In short: `make dev-api` and
`make dev-web` run the backend and a hot-reloading UI, `make test` and
`make lint` run the checks, and `make e2e` builds the image and runs the
end-to-end smoke test. The design contract is
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## License

Apache-2.0. Container images include
[jellyfin-ffmpeg](https://github.com/jellyfin/jellyfin-ffmpeg) (GPL).
