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
| `crates/szalinski-core` | Shared types; their JSON shape is the API |
| `crates/szalinski-hwdetect` | CPU/memory/GPU detection, encoder test encodes, job-count recommendation |
| `crates/szalinski-scanner` | Folder walking, ffprobe, folder watching |
| `crates/szalinski-worker` | Transcode plans, ffmpeg runs, verification, safe replacement |
| `crates/szalinski-server` | The `szalinski` binary: HTTP API, WebSocket, queue, UI hosting |
| `web/` | Next.js UI, exported as static files to `web/out` |
| `docker/`, `Dockerfile`, `docker-compose*.yml` | Container image and examples |
| `unraid/` | Community Applications template and icon |
| `scripts/` | Test and demo media generators, the end-to-end smoke tests, the screenshot script, and the release rules (`release-channel.sh`, `release-notes.sh`, tested by `test-release-channel.sh`) |
| `docs/` | Architecture, Unraid and hardware guides, and `screenshots/` |
| `CHANGELOG.md` | What changed in each release; the source of the GitHub Release notes |
| `.github/workflows/` | CI and the image release |

## Everyday commands

`make` lists every target. The main ones:

| Command | Does |
|---|---|
| `make dev-api` | Runs the backend on :8080 (`cargo run -p szalinski-server -- --data-dir ./data --dev-cors`) |
| `make dev-web` | Runs `next dev` on :3000 with `NEXT_PUBLIC_API_URL=http://localhost:8080` |
| `make test` | `cargo test --workspace` and the web unit tests (`pnpm test`) |
| `make lint` | `cargo fmt --check`, clippy with `-D warnings`, eslint, `tsc --noEmit` and shellcheck |
| `make build` | Static UI in `web/out` plus the release binary |
| `make run` | Builds, then serves UI and API together from `./target/release/szalinski` on :8080 |
| `make test-media` | Writes a synthetic library to `./media` |
| `make docker` | Builds the image `szalinski:dev` |
| `make test-docker` | Builds the image and runs the entrypoint tests |
| `make test-release` | Checks the release rules without Docker or GitHub: which tags a release moves (`stable` never moves backwards) and the release notes |
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
cargo run -p szalinski-server -- --data-dir ./data --dev-cors
cd web && pnpm install && NEXT_PUBLIC_API_URL=http://localhost:8080 pnpm dev
```

Server options are flags with environment-variable fallbacks
(`cargo run -p szalinski-server -- --help`); the table is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#configuration-server). To work on
the UI alone, [web/README.md](web/README.md) describes a mock API with sample
data and the phone layout check (`pnpm e2e:layout`).

### Test media

```sh
scripts/make-test-media.sh ./media 6      # directory, clip length in seconds
```

creates, in about five seconds, a small library that covers the common
cases: H.264 MP4 with two audio languages, 1080p H.264 MKV with 5.1 AC-3 and
subtitles, 10-bit HEVC, interlaced MPEG-2 in MPEG-TS, an MPEG-4 AVI with odd
dimensions, an audio-only FLAC, a truncated MKV and a text file named `.mp4`.
Rust tests generate the same kind of media into temporary folders.

Tests that make a folder stop answering, as a hung network share does, use the
`test-hooks` feature of `szalinski-worker` (`slow_fs::hang`,
`finalize::hold`). The crates' own tests turn it on, so `cargo test --workspace`
needs nothing extra; it is never part of the release build or the image.

## Docker image

```sh
make docker                                   # szalinski:dev for linux/amd64
docker buildx build --platform linux/amd64,linux/arm64 -t szalinski:multi .
```

The first build takes 10 to 15 minutes on four cores and about 8 GB of disk
space (`docker builder prune -af` gives back about 3 GB afterwards); the image
is about 600 MB. Later builds reuse the cached steps: a change
to the web pages alone takes about two minutes.
`make docker` does not pass `VERSION`, so the image's build label is `dev`; for
a build that others will install (README, [Build it
yourself](README.md#build-it-yourself)) use
`docker build -t szalinski:local --build-arg VERSION=local .`.

The build compiles the UI and the Rust binary on the build machine's own
architecture and cross-compiles the binary for arm64, so an arm64 image needs
emulation only for the final `apt-get` step. To build and try one on an amd64
machine, register QEMU once (the host needs `binfmt_misc` mounted), build with
`--load` and run the image with `--platform`:

```sh
docker run --privileged --rm tonistiigi/binfmt --install arm64
docker buildx build --platform linux/arm64 --load -t szalinski:arm64 .
docker run --rm --platform linux/arm64 szalinski:arm64 --version
scripts/e2e-smoke.sh szalinski:arm64      # works, slowly, under emulation
```

Useful build arguments:
`CARGO_BUILD_JOBS` (limit compile parallelism), `JELLYFIN_FFMPEG_VERSION`
(pin ffmpeg), `VERSION`/`REVISION` (image metadata).

The entrypoint (`docker/entrypoint.sh`) handles PUID/PGID/UMASK, GPU device
groups and dropping privileges; `docker run --rm szalinski:dev id` shows the
result, and `docker run --rm szalinski:dev --help` reaches the binary.
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
make e2e                                  # build, then test szalinski:dev
scripts/e2e-smoke.sh ghcr.io/thekozugroup/szalinski:latest
E2E_URL=http://127.0.0.1:8080 scripts/e2e-smoke.sh   # against a running dev server

make e2e-browser                          # build, then drive the first run in Chromium
scripts/e2e-browser.sh szalinski:dev
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
the file's verification report, checks that History lists a renamed file under
its new name with *Was <old name>* beneath it, opens Settings > Hardware, and
fails on any console or server error. It needs Playwright (see Prerequisites).
It takes about two minutes on four cores. `E2E_GOAL`, `E2E_TIMEOUT` (the 300 s
limit for conversions), `E2E_PORT`, `E2E_KEEP=1` and `E2E_SCREENSHOTS=<dir>` (a
picture of each step) tune it.

### Screenshots

`docs/screenshots/*.png` are real pages of a running container: dark theme,
1440x900, plus one phone shot (390x844 at 2x). They show real conversions of
the demo library, not mock data. To retake them:

```sh
scripts/make-demo-media.sh /srv/demo-media            # eleven files, about 175 MB
docker run -d --name demo -p 8080:8080 -v /srv/demo-media:/media -v "$(mktemp -d)":/config \
  -e PUID="$(id -u)" -e PGID="$(id -g)" szalinski:dev
node scripts/take-screenshots.mjs http://127.0.0.1:8080
```

The script goes through the first-run screens on a fresh server, adds a second
library, waits until a conversion is part-way with some files already done, and
saves `setup`, `overview`, `queue`, `phone`, `job` (a verified file's checks) and
`hardware` (Settings > Hardware, the page headed *This machine*). It takes about
ten minutes on four cores. Make the demo media as the user the container runs
as (as above), or the job page shows a note about a changed file owner. Shrink
the results before committing them
(`pngquant --force --ext .png --quality 60-85 docs/screenshots/*.png`, or any
256-colour quantizer): each should be well under 150 KB. Retake only the shots
whose screen has visibly changed since the last time.

## CI and releases

- `.github/workflows/ci.yml` runs on pull requests and on pushes to `main`:
  workflow, shell-script and template lint (actionlint via the
  `rhysd/actionlint` image, shellcheck, xmllint on the Unraid template,
  `docker compose config` on the compose files, and
  `scripts/test-release-channel.sh`, the release rules below), Rust (fmt,
  clippy, tests with ffmpeg installed), web (lint, typecheck, unit tests,
  static build) and Docker (amd64 image, entrypoint tests,
  `scripts/e2e-smoke.sh` and `scripts/e2e-browser.sh` against the built image).
- `.github/workflows/release.yml` runs on pushes to `main`, on `v*` tags and
  by hand. It builds the amd64 image, runs the entrypoint tests and both
  end-to-end tests against it, and only then pushes a multi-arch (amd64 +
  arm64) image to `ghcr.io/<owner>/szalinski`. Which tags a run publishes is
  in [Update channels](#update-channels) below; a version tag also gets a GitHub
  Release ([Cutting a release](#cutting-a-release)). The arm64 image is
  cross-compiled (only the final `apt-get` step runs under emulation) and is not
  run in CI.
- There are two version numbers. The server reports the `version` in
  `Cargo.toml` (`[workspace.package]`) in `/api/health`, `/api/system` and its
  "is running" log line. The image version is `1.2.3` for a tag,
  `main-<short sha>` for a build from `main` and `<branch>-<short sha>` for a
  manual run (`dev` for a local build without `VERSION`); it is in the OCI
  version label, the `SZALINSKI_VERSION` variable, the first line of the
  container log and *About* at the bottom of Settings, where it is called the
  *build*, next to the server's version: `Szalinski 0.3.0 (build
  main-1a2b3c4)` in the log, `Szalinski 0.3.0 · build main-1a2b3c4` in
  *About*. A bug report that quotes it names the exact commit. To release, set
  `version` in `Cargo.toml` to `1.2.3`, commit, then tag `v1.2.3`; the workflow
  refuses a tag that does not match (see [Cutting a release](#cutting-a-release)).
- To publish an image from a branch before merging it (for example to try it
  on an Unraid server), open **Actions > Release > Run workflow** and pick the
  branch. The same tests run, and the image is pushed as `edge` and
  `sha-<short>` (image version `<branch>-<short sha>`); `latest` and `stable`
  are not touched. On Unraid, set the container's Repository to
  `ghcr.io/<owner>/szalinski:edge`. Afterwards set Repository back to
  `:stable`, or to `:latest` once the branch is merged and `main`'s own release
  run has published it.
- The Unraid template's `TemplateURL` and `Icon` point at the `main` branch
  (`unraid/szalinski.xml`, `unraid/szalinski.png`), so a template or icon
  change reaches users only once it is merged. A template tried from a branch
  must be downloaded from that branch's raw URL by hand.
- The first image ever published, including a first `edge` from a branch,
  creates the package as **private** (GitHub copies the repository's access
  rules to a new package, but not its visibility). Make it public once:
  GitHub > Packages > szalinski > Package settings > Change visibility >
  Public. Until then Unraid and Docker are refused when pulling it. The
  workflow run's summary repeats this.

### Update channels

The image tag is the update channel: a container follows whatever its tag
follows. `latest` follows every build of `main` and is not a release channel;
`stable` is.

| Trigger | Image tags pushed | GitHub Release |
|---|---|---|
| Push to `main` | `latest`, `sha-<short>` | none |
| Tag `v1.2.3` | `1.2.3`, `sha-<short>`, then the moving tags below that this release is entitled to | yes, as the latest release if it moved `stable` |
| Tag `v1.2.0-rc.1` (any tag with a `-` suffix, a prerelease) | `1.2.0-rc.1`, `sha-<short>`; no moving tag | yes, marked as a pre-release |
| Manual run on another branch | `edge`, `sha-<short>` | none |
| Manual run on a tag | as for that tag; never moves a moving tag backwards | only if it has none yet |

The moving tags of a release (`scripts/release-channel.sh moving-tags`):

- `stable` only if this is the newest release of all;
- `1.2` only if it is the newest release of the 1.2 line, and `1` (from 1.0
  on; `v0.x` has no major-only tag) only if it is the newest of the 1.x line.

A "release" here is a tag that is a strict `vMAJOR.MINOR.PATCH` version
without a prerelease suffix, and "newest" compares the numbers, not the text,
so `v0.10.0` is newer than `v0.9.9`. Other tags (`v1`, `vfoo`) are ignored by
the rules, and the workflow refuses to build a `v*` tag that is not a valid
version. The tags `1.2.3` and `sha-<short>` are exact and never move.

### Cutting a release

A release is a version tag on `main`. Nothing else moves `stable`.

1. Make sure the commit you want to ship is on `main` and green: CI, and the
   **Release** run that `main` itself triggers.
2. Prepare the release in a pull request:
   - In `CHANGELOG.md`, move the entries of `## [Unreleased]` into a new
     `## [X.Y.Z] - YYYY-MM-DD` section, leave an empty `## [Unreleased]` above
     it and update the links at the bottom. The release workflow copies this
     section into the GitHub Release; without it the notes list commits only and
     the run says so.
   - Set `version` in `Cargo.toml` (`[workspace.package]`) to `X.Y.Z` and
     refresh the lock file with `cargo update --workspace`, which changes only
     the five workspace crates' versions (CI builds with `--locked`).
   - Merge it.
3. Tag the merge commit and push the tag:

   ```sh
   git switch main && git pull
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```

   A prerelease works the same with a suffix: set `version = "X.Y.Z-rc.1"` in
   `Cargo.toml` and push `vX.Y.Z-rc.1`. The final `vX.Y.Z` needs `X.Y.Z` there.
4. Watch **Actions > Release**. For a version tag the workflow:
   1. refuses a tag that is not a valid version, or that does not match
      `version` in `Cargo.toml`, before building anything;
   2. builds the amd64 image and runs the entrypoint tests and both end-to-end
      tests against it;
   3. pushes the multi-arch image as `X.Y.Z` and `sha-<short>`;
   4. reads the tags that exist at that moment and, for a release (not a
      prerelease), copies the image to `stable`, `X.Y` and `X` as far as this
      release is the newest of each (the same image, by digest, checked after);
   5. in a second job, the only one allowed to write repository contents,
      creates the GitHub Release for the tag: the `CHANGELOG.md` section, the
      commits since the previous release (merge commits left out, at most 100),
      a link to the full comparison and the image tags. It is marked as a
      pre-release for a prerelease, and as the latest release only if it moved
      `stable`. A release that already exists is left as it is, so a re-run
      cannot overwrite notes you edited.
5. Run the checks under [Before announcing a release](#before-announcing-a-release).

**The first release.** `stable` is created by the first version tag. Until a
tag such as `v0.3.0` has been pushed and its Release run has finished,
`ghcr.io/thekozugroup/szalinski:stable` does not exist, and pulling it fails
with *manifest unknown*. The Unraid template's Repository is `:stable`, so push
the first release (and make the package public) before the template reaches
`main` and Community Applications, or the first install from it fails.
`Cargo.toml` says `0.3.0` today, so the first tag is `v0.3.0` unless you change
it.

**How `stable` is protected from moving backwards.**

- The rule is in one place, `scripts/release-channel.sh`, and compares versions
  numerically (`v0.10.0` beats `v0.9.9`). `scripts/test-release-channel.sh`
  tests it, with `v0.2.0`, `v0.3.0`, `v0.3.1-rc.1`, `v0.10.0` and `v0.9.9`
  among others, and CI runs it (`make test-release` locally). Only the newest
  of those, `v0.10.0`, takes `stable`: `v0.9.9` and the prerelease do not.
- The decision is made after the build, right before the tags are moved, from
  the tags that exist then (read from the GitHub API), not from those at the
  start of the run. If `v0.3.1` is tagged while `v0.3.0` is still building,
  `v0.3.0` leaves `stable` alone, whichever finishes first.
- Re-running an older release (from the Actions page) republishes its exact
  tag and cannot move `stable`, `X.Y` or `X` backwards. A prerelease never
  moves any of them.
- The cost: the newest *tag* decides, even if its own run failed. If the
  newest version's run fails, `stable` stays where it was, and older tags cannot
  move it. Re-run the failed run, or cut the next patch release with the fix.
- Never re-tag or force-push a version that was published. Containers that
  follow `stable` update by themselves, and the database of a newer version
  cannot be read by an older one. Fix forward with a new patch release. To put
  `stable` back on an older image after a bad release anyway, do it by hand
  (`docker login ghcr.io`, then `docker buildx imagetools create -t
  ghcr.io/<owner>/szalinski:stable ghcr.io/<owner>/szalinski:X.Y.Z`); the
  workflow will not.

### Before announcing a release

Everything the README and `docs/UNRAID.md` tell a new user to download or pull
exists only after these steps, in this order. Check them from a machine (or a
shell) that is not logged in to GitHub or GHCR, because a private package
and a logged-in session hide the problem:

1. Merge to `main` (or push the version tag) and wait for the **Release** run
   to finish green: it pushes `:latest` (or the version tags and `:stable`).
2. The first time ever: set the package to **Public** (GitHub > Packages >
   szalinski > Package settings > Change visibility).
3. Check the image and the raw files the docs point at:

   ```sh
   docker logout ghcr.io
   docker pull ghcr.io/thekozugroup/szalinski:latest      # not "denied"
   docker pull ghcr.io/thekozugroup/szalinski:stable      # needs a version tag; not "manifest unknown"
   for tag in stable X.Y.Z; do                              # the same digest twice
     docker buildx imagetools inspect "ghcr.io/thekozugroup/szalinski:$tag" --format '{{json .Manifest}}' | jq -r .digest
   done
   raw=https://raw.githubusercontent.com/thekozugroup/Szalinski/main
   for f in unraid/szalinski.xml unraid/szalinski.png docker-compose.yml .env.example; do
     curl -fsSL -o /dev/null -w "%{http_code}  $f\n" "$raw/$f" || echo "FAILED  $f"
   done
   ```

   All four must answer `200`. For a fresh Compose install, also run the
   README's Compose steps in an empty folder and confirm `docker compose up -d`
   starts `ghcr.io/thekozugroup/szalinski:latest` (`docker compose config`
   shows the image), and open the web UI.
4. For a version tag, open the project's Releases page: the release has its
   notes, a prerelease is marked as one, and only a release that moved
   `stable` carries the *Latest* badge.
5. Only then announce it. Until it is done, the README and UNRAID.md callouts
   tell people to use the branch's raw URLs and the `:edge` image, or to build
   the image themselves.

Run the workflow checks locally before pushing a change to `.github/` or
`scripts/`:

```sh
docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:1.7.12 -color
shellcheck docker/*.sh scripts/*.sh
xmllint --noout unraid/szalinski.xml
scripts/test-release-channel.sh       # make test-release
```

## Conventions

- Commits follow [Conventional Commits](https://www.conventionalcommits.org)
  (`feat:`, `fix:`, `docs:`, ...).
- Rust: no `unwrap`/`expect` on runtime paths, no blocking work on the async
  runtime, `cargo fmt` and clippy clean. Errors shown to users are complete,
  plain-language sentences.
- Changing a type in `szalinski-core` changes the API: update
  `web/src/lib/types.ts` and `docs/ARCHITECTURE.md` in the same change.

## License

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE).
