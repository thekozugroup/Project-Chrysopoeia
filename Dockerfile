# Stage 1: Build Rust backend
FROM rust:1.85-bookworm AS rust-builder
WORKDIR /app
# Install ffmpeg for fallback encoding
RUN apt-get update && apt-get install -y ffmpeg libvulkan-dev
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release

# Stage 2: Build Next.js frontend
FROM node:22-bookworm AS web-builder
WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY web/ .
RUN pnpm build

# Stage 3: Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ffmpeg libvulkan1 ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=rust-builder /app/target/release/chrysopeia-server ./
COPY --from=web-builder /app/web/.next/standalone ./web/
COPY --from=web-builder /app/web/.next/static ./web/.next/static
COPY --from=web-builder /app/web/public ./web/public
EXPOSE 3000 8080
ENV RUST_LOG=info
ENV DATABASE_URL=sqlite:///data/chrysopeia.db
ENV MEDIA_PATH=/media
CMD ["./chrysopeia-server", "--port", "8080", "--web-dir", "./web", "--db", "/data/chrysopeia.db"]
