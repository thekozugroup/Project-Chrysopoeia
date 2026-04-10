# Chrysopeia

Self-hosted media transcoding and management server. Converts your media library to modern formats (AV1, HEVC) with hardware acceleration support.

## Quick Start

```bash
cp .env.example .env
# Edit .env to set your MEDIA_PATH
docker compose up -d
```

The web UI is available at `http://localhost:3000` and the API at `http://localhost:8080`.

## Architecture

- **Backend** -- Rust server handling transcoding jobs, media scanning, and the REST API
- **Frontend** -- Next.js web interface for library browsing, job management, and configuration
- **Storage** -- SQLite database for metadata and job state; media files served from a mounted volume

## Features

- Batch transcode to AV1, HEVC, or H.264
- Hardware-accelerated encoding (NVIDIA NVENC, VA-API, Vulkan)
- FFmpeg fallback for software encoding
- Concurrent job queue with configurable parallelism
- Media library scanning and metadata extraction
- Real-time job progress via WebSocket
- Web UI for browsing, filtering, and managing your library

## Development

### Prerequisites

- Rust 1.85+
- Node.js 22+ with pnpm
- FFmpeg

### Running locally

```bash
# Backend
cargo run --release -p chrysopeia-server

# Frontend (separate terminal)
cd web
pnpm install
pnpm dev
```

### Docker build

```bash
docker compose build
docker compose up
```

## GPU Support

The `docker-compose.yml` includes NVIDIA GPU reservations. If you don't have an NVIDIA GPU, remove or comment out the `deploy.resources.reservations.devices` section, or set `HW_ACCEL=none` in your `.env`.

## License

MIT
