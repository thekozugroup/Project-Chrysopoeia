# Contributing

Thanks for helping. This page covers the tools, the everyday commands and how
the pieces fit together. The design contract (API, database, worker
behaviour, hardware detection) is [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md);
product principles are in [PRODUCT.md](PRODUCT.md).

## Prerequisites

- **Rust** 1.88 or newer (edition 2024, `let` chains), via
  [rustup](https://rustup.rs).
- **Node.js** 22 and **pnpm** 10: `corepack enable pnpm` sets pnpm up.
- **ffmpeg and ffprobe** 6 or newer on `PATH`, built with libx264, libx265,
  libsvtav1, libvpx and libopus (any Linux distribution's package or Homebrew
  has them). Tests that need ffmpeg skip themselves when it is missing.
- **Docker** with BuildKit (Docker 23+) for images and the end-to-end tests.
- Optional: `shellcheck` for the shell scripts, and Playwright with Chromium
  (`npm install -g playwright && playwright install chromium`) for the browser
  test and the screenshots.

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
| `scripts/` | Test and demo media generators, the end-to-end smoke tests and the screenshot script |
| `docs/` | Architecture, Unraid and hardware guides, and `screenshots/` |
| `.github/workflows/` | CI and the image release |

## Everyday commands

`make` lists every target. The main ones:

| Command | Does |
|---|---|
| `make dev-api` | Runs the backend on :8080 (`cargo run -p chrysopoeia-server -- --data-dir ./data --dev-cors`) |
| `make dev-web` | Runs `next dev` on :3000 with `NEXT_PUBLIC_API_URL=http://localhost:8080` |
| `make test` | `cargo test --workspace` and the web unit tests (`pnpm test`) |
| `make lint` | `cargo fmt --check`, clippy with `-D warnings`, eslint, `tsc --noEmit` and shellcheck |
| `make build` | Static UI in `web/out` plus the release binary |
| `make run` | Builds, then serves UI and API together from `./target/release/chrysopoeia` on :8080 |
| `make test-media` | Writes a synthetic library to `./media` |
| `make docker` | Builds the image `chrysopoeia:dev` |
| `make test-docker` | Builds the image and runs the entrypoint tests |
| `make e2e` | Builds the image and runs the end-to-end smoke test (API) |
| `make e2e-browser` | Builds the image and drives its first run in a real browser |

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

The first build takes 10 to 15 minutes on four cores and about 8 GB of disk
space (`docker builder prune -af` gives back about 3 GB afterwards); the image
is about 600 MB. Later builds reuse the cached steps: a change
to the web pages alone takes about two minutes.
`make docker` does not pass `VERSION`, so the image's build label is `dev`; for
a build that others will install (README, [Build it
yourself](README.md#build-it-yourself)) use
`docker build -t chrysopoeia:local --build-arg VERSION=local .`.

The build compiles the UI and the Rust binary on the build machine's own
architecture and cross-compiles the binary for arm64, so an arm64 image needs
emulation only for the final `apt-get` step. To build and try one on an amd64
machine, register QEMU once (the host needs `binfmt_misc` mounted), build with
`--load` and run the image with `--platform`:

```sh
docker run --privileged --rm tonistiigi/binfmt --install arm64
docker buildx build --platform linux/arm64 --load -t chrysopoeia:arm64 .
docker run --rm --platform linux/arm64 chrysopoeia:arm64 --version
scripts/e2e-smoke.sh chrysopoeia:arm64      # works, slowly, under emulation
```

Useful build arguments:
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

## End-to-end tests

Two scripts test a built image the way it is used. Both start it with an empty
`/config` and a small synthetic library made by the image's own ffmpeg
(`scripts/make-test-media.sh`), as a non-root user so the privilege drop is
exercised, and clean up after themselves.

```sh
make e2e                                  # build, then test chrysopoeia:dev
scripts/e2e-smoke.sh ghcr.io/thekozugroup/chrysopoeia:latest
E2E_URL=http://127.0.0.1:8080 scripts/e2e-smoke.sh   # against a running dev server

make e2e-browser                          # build, then drive the first run in Chromium
scripts/e2e-browser.sh chrysopoeia:dev
```

`scripts/e2e-smoke.sh` talks to the API. It adds the library with the *Plays
everywhere* goal, waits until every file is in the library (brand-new files are
first left alone for 20 seconds, as if still being copied) and the queue is
idle, then checks that real videos ended up done or skipped (never failed),
that done jobs carry a passing verification report, that outputs really are
H.264, that no original was lost or temporary file left behind, and that the
container reports healthy. It takes a few minutes on four cores.
`E2E_TIMEOUT`, `E2E_GOAL`, `E2E_MAX_JOBS`, `E2E_PORT` (fixed host port instead
of a random one) and `E2E_KEEP=1` (keep the container and files for
inspection) tune it.

`scripts/e2e-browser.sh` runs `web/e2e/smoke.mjs`: a real browser goes through
the welcome, folder and goal screens, waits for a file to be converted and
verified, sees the live card and the saved space appear without a reload, opens
the file's verification report and Settings > Hardware, and fails on any
console or server error. It needs Playwright (see Prerequisites). It takes
about a minute. `E2E_GOAL`, `E2E_TIMEOUT`, `E2E_PORT`, `E2E_KEEP=1` and
`E2E_SCREENSHOTS=<dir>` (a picture of each step) tune it.

### Screenshots

`docs/screenshots/*.png` are real pages of a running container: dark theme,
1440x900, plus one phone shot (390x844 at 2x). They show real conversions of
the demo library, not mock data. To retake them:

```sh
scripts/make-demo-media.sh /srv/demo-media            # eleven files, about 175 MB
docker run -d --name demo -p 8080:8080 -v /srv/demo-media:/media -v "$(mktemp -d)":/config \
  -e PUID="$(id -u)" -e PGID="$(id -g)" chrysopoeia:dev
node scripts/take-screenshots.mjs http://127.0.0.1:8080
```

The script goes through the first-run screens on a fresh server, adds a second
library, waits until a conversion is part-way with some files already done, and
saves `setup`, `overview`, `queue`, `phone`, `job` (a verified file's checks) and
`hardware` (Settings > Hardware, the page headed *This machine*). It takes about
twenty minutes on four cores. Shrink the results before committing them
(`pngquant --force --ext .png --quality 60-85 docs/screenshots/*.png`, or any
256-colour quantizer): each should be well under 150 KB.

## CI and releases

- `.github/workflows/ci.yml` runs on pull requests and on pushes to `main`:
  workflow, shell-script and template lint (actionlint via the
  `rhysd/actionlint` image, shellcheck, xmllint on the Unraid template,
  `docker compose config` on the compose files), Rust (fmt, clippy, tests with
  ffmpeg installed), web (lint, typecheck, unit tests, static build) and Docker
  (amd64 image, entrypoint tests, `scripts/e2e-smoke.sh` and
  `scripts/e2e-browser.sh` against the built image).
- `.github/workflows/release.yml` runs on pushes to `main`, on `v*` tags and
  by hand. It builds the amd64 image, runs the entrypoint tests and both
  end-to-end tests against it, and only then pushes a multi-arch (amd64 +
  arm64) image to `ghcr.io/<owner>/chrysopoeia`: a push to `main` is tagged
  `latest` and `sha-<short>`; a tag `v1.2.3` is tagged `1.2.3`, `1.2`, `1` and
  `sha-<short>` (a `v0.*` tag gets no major-only tag: `v0.2.3` is tagged `0.2.3`,
  `0.2` and `sha-<short>`). A version tag never moves `latest`, which always
  follows `main`. The arm64 image is cross-compiled (only the final `apt-get` step runs
  under emulation) and is not run in CI.
- There are two version numbers. The server reports the `version` in
  `Cargo.toml` (`[workspace.package]`) in `/api/health`, `/api/system` and its
  "is running" log line. The image version is `1.2.3` for a tag,
  `main-<short sha>` for a build from `main` and `<branch>-<short sha>` for a
  manual run (`dev` for a local build without `VERSION`); it is in the OCI
  version label, the `CHRYSOPOEIA_VERSION` variable, the first line of the
  container log and *About* at the bottom of Settings, where it is called the
  *build*, next to the server's version: `Chrysopoeia 0.2.0 (build
  main-1a2b3c4)` in the log, `Chrysopoeia 0.2.0 · build main-1a2b3c4` in
  *About*. A bug report that quotes it names the exact commit. To release, set
  `version` in `Cargo.toml` to `1.2.3`, commit, then tag `v1.2.3`; the workflow
  refuses a tag that does not match.
- To publish an image from a branch before merging it (for example to try it
  on an Unraid server), open **Actions > Release > Run workflow** and pick the
  branch. The same tests run, and the image is pushed as `edge` and
  `sha-<short>` (image version `<branch>-<short sha>`); `latest` is not
  touched. On Unraid, set the container's Repository to
  `ghcr.io/<owner>/chrysopoeia:edge`. After the merge, `main`'s own release run
  publishes `latest`; set Repository back to it.
- The Unraid template's `TemplateURL` and `Icon` point at the `main` branch
  (`unraid/chrysopoeia.xml`, `unraid/chrysopoeia.png`), so a template or icon
  change reaches users only once it is merged. A template tried from a branch
  must be downloaded from that branch's raw URL by hand.
- The first image ever published, including a first `edge` from a branch,
  creates the package as **private** (GitHub copies the repository's access
  rules to a new package, but not its visibility). Make it public once:
  GitHub > Packages > chrysopoeia > Package settings > Change visibility >
  Public. Until then Unraid and Docker are refused when pulling it. The
  workflow run's summary repeats this.

### Before announcing a release

Everything the README and `docs/UNRAID.md` tell a new user to download or pull
exists only after these steps, in this order. Check them from a machine (or a
shell) that is not logged in to GitHub or GHCR, because a private package
and a logged-in session hide the problem:

1. Merge to `main` (or push the version tag) and wait for the **Release** run
   to finish green: it pushes `:latest` (or the version tags).
2. The first time ever: set the package to **Public** (GitHub > Packages >
   chrysopoeia > Package settings > Change visibility).
3. Check the image and the raw files the docs point at:

   ```sh
   docker logout ghcr.io
   docker pull ghcr.io/thekozugroup/chrysopoeia:latest      # not "denied"
   raw=https://raw.githubusercontent.com/thekozugroup/Project-Chrysopoeia/main
   for f in unraid/chrysopoeia.xml unraid/chrysopoeia.png docker-compose.yml .env.example; do
     curl -fsSL -o /dev/null -w "%{http_code}  $f\n" "$raw/$f" || echo "FAILED  $f"
   done
   ```

   All four must answer `200`. For a fresh Compose install, also run the
   README's Compose steps in an empty folder and confirm `docker compose up -d`
   starts `ghcr.io/thekozugroup/chrysopoeia:latest` (`docker compose config`
   shows the image), and open the web UI.
4. Only then announce it. Until it is done, the README and UNRAID.md callouts
   tell people to use the branch's raw URLs and the `:edge` image, or to build
   the image themselves.

Run the workflow checks locally before pushing a change to `.github/`:

```sh
docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:1.7.12 -color
shellcheck docker/*.sh scripts/*.sh
xmllint --noout unraid/chrysopoeia.xml
```

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
[Apache License 2.0](LICENSE).
