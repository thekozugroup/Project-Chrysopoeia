# Contributing

## Prerequisites

- Rust 1.85+
- Node.js 22+ with pnpm
- FFmpeg

## Running locally

```bash
# Backend
cargo run --release -p chrysopoeia-server

# Frontend (separate terminal)
cd web && pnpm install && pnpm dev
```

## Docker

```bash
cp .env.example .env
docker compose up -d
```

The web UI is at `http://localhost:3000`, API at `http://localhost:8080`.

## GPU Support

The `docker-compose.yml` includes NVIDIA GPU reservations. Remove or comment the `deploy.resources.reservations.devices` section if you don't have an NVIDIA GPU, or set `HW_ACCEL=none`.

## License

Apache-2.0
