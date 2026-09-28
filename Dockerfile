# syntax=docker/dockerfile:1
#
# Chrysopoeia: one container, one port. Multi-arch (linux/amd64, linux/arm64).
#
#   docker build -t chrysopoeia .
#   docker buildx build --platform linux/amd64,linux/arm64 -t chrysopoeia .
#
# The web UI and the Rust binary are built on the build machine's own platform
# ($BUILDPLATFORM); the binary is cross-compiled for $TARGETPLATFORM, so an
# arm64 image needs no emulation to compile. Only the final stage's apt-get
# runs under emulation when building for a foreign architecture.
#
# Build arguments:
#   CARGO_BUILD_JOBS          limit parallel rustc jobs (default: all cores)
#   CARGO_CHEF_VERSION        cargo-chef used to cache compiled dependencies
#   JELLYFIN_FFMPEG_VERSION   pin jellyfin-ffmpeg7 (e.g. 7.1.4-3-bookworm); default: newest
#   VERSION, REVISION, CREATED  image metadata, set by CI

ARG RUST_VERSION=1
ARG NODE_VERSION=22

# ---------------------------------------------------------------------------
# Web UI: Next.js static export -> /src/web/out
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS web
# pnpm version the lockfile is written with. A "packageManager" field in
# web/package.json takes precedence.
ARG PNPM_VERSION=10.33.0
ENV NEXT_TELEMETRY_DISABLED=1 \
    COREPACK_ENABLE_DOWNLOAD_PROMPT=0
WORKDIR /src/web
RUN corepack enable pnpm && corepack install -g "pnpm@${PNPM_VERSION}"
# Manifests first so dependency installs are cached until they change.
COPY web/package.json web/pnpm-*.yaml ./
RUN --mount=type=cache,id=chrysopoeia-pnpm,target=/pnpm/store \
    pnpm install --frozen-lockfile --store-dir /pnpm/store
COPY web/ ./
RUN pnpm build \
 && if [ ! -f out/index.html ]; then \
      echo 'web/out/index.html is missing: next.config.ts must set output: "export".' >&2; \
      exit 1; \
    fi

# ---------------------------------------------------------------------------
# Server binary: cross-compiled for the target platform -> /out/chrysopoeia
#
# Dependencies are compiled in a layer of their own (cargo-chef) that depends
# only on the Cargo manifests and Cargo.lock, so a source change recompiles
# just Chrysopoeia's crates. Unlike RUN cache mounts, layers are kept by CI's
# GitHub Actions cache (type=gha).
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS rust-chef
ARG CARGO_CHEF_VERSION=0.1.78
ARG CARGO_BUILD_JOBS
ENV CARGO_TERM_COLOR=never
RUN --mount=type=cache,id=chrysopoeia-cargo-registry,target=/usr/local/cargo/registry \
    set -eu; \
    jobs="${CARGO_BUILD_JOBS:-}"; \
    unset CARGO_BUILD_JOBS; \
    cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}" ${jobs:+--jobs "$jobs"}
WORKDIR /src

# The recipe: the workspace's manifests and lockfile, with the sources removed.
FROM rust-chef AS rust-plan
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,id=chrysopoeia-cargo-registry,target=/usr/local/cargo/registry \
    cargo chef prepare --recipe-path /recipe.json

FROM rust-chef AS rust
ARG TARGETARCH
ARG BUILDARCH
# Cross linkers and C compilers (sqlx bundles SQLite's C sources). On a native
# build these name the host compiler, which Debian also installs under the
# target-prefixed name.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
    SQLX_OFFLINE=true
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu;  cross="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
      arm64) triple=aarch64-unknown-linux-gnu; cross="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
      *) echo "Unsupported target architecture: $TARGETARCH (supported: amd64, arm64)" >&2; exit 1 ;; \
    esac; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends $cross; \
      rm -rf /var/lib/apt/lists/*; \
    fi; \
    rustup target add "$triple"; \
    echo "$triple" > /rust-target
ARG CARGO_BUILD_JOBS
# 1. Dependencies only (cached until Cargo.toml or Cargo.lock change).
COPY --from=rust-plan /recipe.json /recipe.json
RUN --mount=type=cache,id=chrysopoeia-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=chrysopoeia-cargo-git,target=/usr/local/cargo/git \
    set -eu; \
    triple="$(cat /rust-target)"; \
    jobs="${CARGO_BUILD_JOBS:-}"; \
    unset CARGO_BUILD_JOBS; \
    cargo chef cook --release --locked --target "$triple" --recipe-path /recipe.json \
        -p chrysopoeia-server --bin chrysopoeia ${jobs:+--jobs "$jobs"}
# 2. Chrysopoeia itself.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,id=chrysopoeia-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=chrysopoeia-cargo-git,target=/usr/local/cargo/git \
    set -eu; \
    triple="$(cat /rust-target)"; \
    jobs="${CARGO_BUILD_JOBS:-}"; \
    unset CARGO_BUILD_JOBS; \
    cargo build --release --locked --target "$triple" -p chrysopoeia-server --bin chrysopoeia ${jobs:+--jobs "$jobs"}; \
    install -D -m 0755 "target/$triple/release/chrysopoeia" /out/chrysopoeia

# ---------------------------------------------------------------------------
# Runtime: Debian + jellyfin-ffmpeg (NVENC, QSV, VA-API with bundled Intel and
# AMD drivers, AMF, V4L2, Rockchip on arm64) + the server and UI.
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
ARG JELLYFIN_FFMPEG_VERSION
ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED
ARG DEBIAN_FRONTEND=noninteractive

LABEL org.opencontainers.image.title="Chrysopoeia" \
      org.opencontainers.image.description="Self-hosted media transcoder: pick a folder and a goal; every file is verified before it replaces the original." \
      org.opencontainers.image.url="https://github.com/thekozugroup/Project-Chrysopoeia" \
      org.opencontainers.image.source="https://github.com/thekozugroup/Project-Chrysopoeia" \
      org.opencontainers.image.documentation="https://github.com/thekozugroup/Project-Chrysopoeia#readme" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.vendor="thekozugroup" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}" \
      net.unraid.docker.webui="http://[IP]:[PORT:8080]/" \
      net.unraid.docker.icon="https://raw.githubusercontent.com/thekozugroup/Project-Chrysopoeia/main/unraid/chrysopoeia.png"

# jellyfin-ffmpeg7 ships its own VA-API drivers (Intel iHD and i965, AMD
# radeonsi) and the Intel oneVPL/MSDK runtimes under /usr/lib/jellyfin-ffmpeg,
# so no distro GPU driver packages are needed. pciutils gives the Hardware
# page real GPU names; tzdata makes TZ (active hours) work.
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl tini tzdata pciutils; \
    install -d -m 0755 /etc/apt/keyrings; \
    curl -fsSL https://repo.jellyfin.org/jellyfin_team.gpg.key -o /etc/apt/keyrings/jellyfin.asc; \
    printf 'Types: deb\nURIs: https://repo.jellyfin.org/debian\nSuites: bookworm\nComponents: main\nArchitectures: %s\nSigned-By: /etc/apt/keyrings/jellyfin.asc\n' \
        "$(dpkg --print-architecture)" > /etc/apt/sources.list.d/jellyfin.sources; \
    apt-get update; \
    apt-get install -y --no-install-recommends "jellyfin-ffmpeg7${JELLYFIN_FFMPEG_VERSION:+=$JELLYFIN_FFMPEG_VERSION}"; \
    ln -s /usr/lib/jellyfin-ffmpeg/ffmpeg /usr/local/bin/ffmpeg; \
    ln -s /usr/lib/jellyfin-ffmpeg/ffprobe /usr/local/bin/ffprobe; \
    if [ -x /usr/lib/jellyfin-ffmpeg/vainfo ]; then ln -s /usr/lib/jellyfin-ffmpeg/vainfo /usr/local/bin/vainfo; fi; \
    apt-get clean; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/* /var/log/apt /var/log/dpkg.log; \
    ffmpeg -hide_banner -version > /dev/null; \
    ffprobe -hide_banner -version > /dev/null; \
    command -v setpriv; \
    # The entrypoint moves this user and group to PUID/PGID at startup.
    groupadd --gid 1000 chrysopoeia; \
    useradd --uid 1000 --gid 1000 --no-create-home --home-dir /nonexistent \
        --shell /usr/sbin/nologin chrysopoeia; \
    # /temp is deliberately not created: scratch files must never land in the
    # container layer (on Unraid that fills docker.img). Mount it or leave it out.
    install -d -m 0775 -o 1000 -g 1000 /config

COPY --from=rust /out/chrysopoeia /usr/local/bin/chrysopoeia
COPY --from=web /src/web/out /app/web
COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/entrypoint.sh

ENV CHRYSOPOEIA_VERSION=${VERSION} \
    DATA_DIR=/config \
    WEB_DIR=/app/web \
    PORT=8080 \
    FFMPEG_PATH=/usr/local/bin/ffmpeg \
    FFPROBE_PATH=/usr/local/bin/ffprobe \
    NVIDIA_DRIVER_CAPABILITIES=compute,video,utility \
    LANG=C.UTF-8

VOLUME ["/config"]
EXPOSE 8080
WORKDIR /config

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl -fsS -o /dev/null "http://127.0.0.1:${PORT:-8080}/api/health" || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/entrypoint.sh"]
CMD ["chrysopoeia"]
