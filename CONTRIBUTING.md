# Contributing

Thanks for helping. This page covers the tools, the everyday commands and how
the pieces fit together. The design contract (API, database, worker
behaviour, hardware detection) is [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md);
product principles are in [PRODUCT.md](PRODUCT.md).

## Prerequisites

- **Rust** 1.85 or newer (edition 2024), via [rustup](https://rustup.rs).
- **Node.js** 22 and **pnpm** 10: `corepack enable pnpm` sets pnpm up.
- **ffmpeg and ffprobe** 6 or newer on `PATH`, built with libx264, libx265,
  libsvtav1, libvpx and libopus (any Linux distribution's package or Homebrew
  has them). Tests that need ffmpeg skip themselves when it is missing.
- **Docker** with BuildKit (Docker 23+) for images and the end-to-end test.
- Optional: `shellcheck` for the shell scripts.

## Repository layout

| Path | What |
|---|---|
| `crates/chrysopoeia-core` | Shared types; their JSON shape is the API |
| `crates/chrysopoeia-hwdetect` | CPU/memory/GPU detection, encoder test encodes, job-count recommendation |
| `crates/chrysopoeia-scanner` | Folder walking, ffprobe, folder watching |
| `crates/chrysopoeia-worker` | Transcode plans, ffmpeg runs, verification, safe replacement |
| `crates/chrysopoeia-server` | The `chrysopoeia` binary: HTTP API, WebSocket, queue, UI hosting |
| `web/` | Next.js UI, exported as static files to `web/out` |
| `docker/`, `Dockerfile`, `docker-compose*.yml` | Container image and examples |
| `unraid/` | Community Applications template and icon |
| `scripts/` | Test media generator and end-to-end smoke test |

## Everyday commands

`make` lists every target. The main ones:

| Command | Does |
|---|---|
| `make dev-api` | Runs the backend on :8080 (`cargo run -p chrysopoeia-server -- --data-dir ./data --dev-cors`) |
| `make dev-web` | Runs `next dev` on :3000 with `NEXT_PUBLIC_API_URL=http://localhost:8080` |
| `make test` | `cargo test --workspace` |
| `make lint` | `cargo fmt --check`, clippy with `-D warnings`, eslint and `tsc --noEmit` |
| `make build` | Static UI in `web/out` plus the release binary |
| `make run` | Builds, then serves UI and API together from `./target/release/chrysopoeia` on :8080 |
| `make test-media` | Writes a synthetic library to `./media` |
| `make docker` | Builds the image `chrysopoeia:dev` |
| `make test-docker` | Builds the image and runs the entrypoint tests |
| `make e2e` | Builds the image and runs the end-to-end smoke test |

### Backend and UI during development

Run the two in separate terminals:

```sh
make dev-api     # API + WebSocket on http://localhost:8080
make dev-web     # UI with hot reload on http://localhost:3000
```

The UI calls the API on the same origin in production. `next dev` runs on
another port, so `make dev-web` sets `NEXT_PUBLIC_API_URL` and `make dev-api`
passes `--dev-cors` to allow it. Without `make`:

```sh
cargo run -p chrysopoeia-server -- --data-dir ./data --dev-cors
cd web && pnpm install && NEXT_PUBLIC_API_URL=http://localhost:8080 pnpm dev
```

Server options are flags with environment-variable fallbacks
(`cargo run -p chrysopoeia-server -- --help`); the table is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#configuration-server).

### Test media

```sh
scripts/make-test-media.sh ./media 6      # directory, clip length in seconds
```

creates, in about five seconds, a small library that covers the common
cases: H.264 MP4 with two audio languages, 1080p H.264 MKV with 5.1 AC-3 and
subtitles, 10-bit HEVC, interlaced MPEG-2 in MPEG-TS, an MPEG-4 AVI with odd
dimensions, an audio-only FLAC, a truncated MKV and a text file named `.mp4`.
Rust tests generate the same kind of media into temporary folders.

## Docker image

```sh
make docker                                   # chrysopoeia:dev for linux/amd64
docker buildx build --platform linux/amd64,linux/arm64 -t chrysopoeia:multi .
```

The build compiles the UI and the Rust binary on the build machine's own
architecture and cross-compiles the binary for arm64, so an arm64 image needs
emulation only for the final `apt-get` step. Useful build arguments:
`CARGO_BUILD_JOBS` (limit compile parallelism), `JELLYFIN_FFMPEG_VERSION`
(pin ffmpeg), `VERSION`/`REVISION` (image metadata).

The entrypoint (`docker/entrypoint.sh`) handles PUID/PGID/UMASK, GPU device
groups and dropping privileges; `docker run --rm chrysopoeia:dev id` shows the
result, and `docker run --rm chrysopoeia:dev --help` reaches the binary.
`make test-docker` (or `docker/test-entrypoint.sh <image>`) runs its
regression tests against an image, with fake GPU device nodes, in about 30
seconds.

The Rust stage compiles dependencies in a separate cargo-chef layer that only
changes with `Cargo.toml`/`Cargo.lock`, so after the first build a source
change recompiles just the workspace crates.

## End-to-end smoke test

```sh
make e2e                                  # build, then test chrysopoeia:dev
scripts/e2e-smoke.sh ghcr.io/thekozugroup/chrysopoeia:latest
E2E_URL=http://127.0.0.1:8080 scripts/e2e-smoke.sh   # against a running dev server
```

It generates the test library with the image's ffmpeg, starts the container,
adds the library with the *Plays everywhere* goal, waits until the queue is
idle, then checks that real videos ended up done or skipped (never failed),
that done jobs carry a passing verification report, that outputs really are
H.264, that no original was lost or temporary file left behind, and that the
container reports healthy. `E2E_TIMEOUT`, `E2E_GOAL`, `E2E_MAX_JOBS` and
`E2E_KEEP=1` (keep the container and files for inspection) tune it.

## CI and releases

- `.github/workflows/ci.yml` runs on pull requests and on pushes to `main`:
  Rust (fmt, clippy, tests with ffmpeg installed), web (lint, typecheck,
  static build) and Docker (amd64 image, entrypoint tests and the smoke test).
- `.github/workflows/release.yml` runs on pushes to `main` and on `v*` tags.
  It builds the amd64 image, runs the entrypoint and smoke tests, and only
  then pushes a multi-arch (amd64 + arm64) image to
  `ghcr.io/<owner>/chrysopoeia` tagged `latest` (main), the version (`1.2.3`,
  `1.2`, `1`) for tags, and `sha-<short>`. The version shown in the app and the
  log is `1.2.3` for a tag and `main-<short sha>` for a build from `main`.
- After the first release, make the package public once on GitHub (Packages >
  chrysopoeia > Package settings > Change visibility), or Unraid and Docker
  will be refused when pulling it.

## Conventions

- Commits follow [Conventional Commits](https://www.conventionalcommits.org)
  (`feat:`, `fix:`, `docs:`, ...).
- Rust: no `unwrap`/`expect` on runtime paths, no blocking work on the async
  runtime, `cargo fmt` and clippy clean. Errors shown to users are complete,
  plain-language sentences.
- Changing a type in `chrysopoeia-core` changes the API: update
  `web/src/lib/types.ts` and `docs/ARCHITECTURE.md` in the same change.

## License

By contributing you agree that your contributions are licensed under the
Apache License 2.0.
