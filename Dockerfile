# =============================================================================
# Stage 1: Build Rust backend
# =============================================================================
FROM rust:1.85-bookworm AS rust-builder
WORKDIR /app

# Install system dependencies for ffmpeg and Vulkan
RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg libvulkan-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY crates/chrysopeia-core/Cargo.toml crates/chrysopeia-core/Cargo.toml
COPY crates/chrysopeia-server/Cargo.toml crates/chrysopeia-server/Cargo.toml
COPY crates/chrysopeia-worker/Cargo.toml crates/chrysopeia-worker/Cargo.toml
COPY crates/chrysopeia-scanner/Cargo.toml crates/chrysopeia-scanner/Cargo.toml
COPY crates/chrysopeia-hwdetect/Cargo.toml crates/chrysopeia-hwdetect/Cargo.toml

# Create dummy source files so cargo can resolve the workspace and cache deps
RUN mkdir -p crates/chrysopeia-core/src && echo "pub fn _dummy() {}" > crates/chrysopeia-core/src/lib.rs \
    && mkdir -p crates/chrysopeia-server/src && echo "fn main() {}" > crates/chrysopeia-server/src/main.rs \
    && mkdir -p crates/chrysopeia-worker/src && echo "fn main() {}" > crates/chrysopeia-worker/src/main.rs \
    && mkdir -p crates/chrysopeia-scanner/src && echo "fn main() {}" > crates/chrysopeia-scanner/src/main.rs \
    && mkdir -p crates/chrysopeia-hwdetect/src && echo "fn main() {}" > crates/chrysopeia-hwdetect/src/main.rs

# Build dependencies only (this layer is cached until Cargo.toml/Cargo.lock change)
RUN cargo build --release 2>/dev/null || true

# Remove dummy sources and copy real source code
RUN rm -rf crates/*/src
COPY crates/ crates/

# Build the actual binaries
RUN cargo build --release

# =============================================================================
# Stage 2: Build Next.js frontend
# =============================================================================
FROM node:22-bookworm-slim AS web-builder
WORKDIR /app/web

# Copy package manifests first for dependency caching
COPY web/package.json web/pnpm-lock.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile

# Copy source and build
COPY web/ .
RUN pnpm build

# =============================================================================
# Stage 3: Runtime
# =============================================================================
FROM debian:bookworm-slim AS runtime

LABEL org.opencontainers.image.title="Chrysopeia" \
      org.opencontainers.image.description="Media transcoding server with AV1/HEVC hardware acceleration" \
      org.opencontainers.image.version="0.1.0" \
      org.opencontainers.image.source="https://github.com/michaelwong/chrysopeia" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.authors="Michael Wong"

# Install minimal runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg libvulkan1 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd --gid 1001 chrysopeia \
    && useradd --uid 1001 --gid chrysopeia --shell /bin/false --create-home chrysopeia

# Create data and media directories with correct ownership
RUN mkdir -p /data /media && chown -R chrysopeia:chrysopeia /data /media

WORKDIR /app

# Copy only the binaries we need from the Rust builder
COPY --from=rust-builder /app/target/release/chrysopeia-server ./
COPY --from=rust-builder /app/target/release/chrysopeia-worker ./
COPY --from=rust-builder /app/target/release/chrysopeia-scanner ./

# Copy Next.js standalone output
COPY --from=web-builder /app/web/.next/standalone ./web/
COPY --from=web-builder /app/web/.next/static ./web/.next/static
COPY --from=web-builder /app/web/public ./web/public

# Set ownership for app directory
RUN chown -R chrysopeia:chrysopeia /app

USER chrysopeia

EXPOSE 3000 8080

ENV RUST_LOG=info \
    DATABASE_URL=sqlite:///data/chrysopeia.db \
    MEDIA_PATH=/media

HEALTHCHECK --interval=30s --timeout=10s --start-period=15s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

CMD ["./chrysopeia-server", "--port", "8080", "--web-dir", "./web", "--db", "/data/chrysopeia.db"]
