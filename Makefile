# Chrysopoeia developer tasks. Run `make` for the list.

API_PORT ?= 8080
WEB_PORT ?= 3000
DATA_DIR ?= ./data
MEDIA_DIR ?= ./media
IMAGE ?= chrysopoeia:dev
PLATFORM ?= linux/amd64

.DEFAULT_GOAL := help
.PHONY: help dev-api dev-web web-install build run test lint fmt docker test-docker e2e test-media clean

help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

dev-api: ## Run the backend on :8080 with CORS open for `make dev-web`
	cargo run -p chrysopoeia-server -- --port $(API_PORT) --data-dir $(DATA_DIR) --web-dir web/out --dev-cors

dev-web: web/node_modules ## Run the UI with hot reload on :3000, talking to `make dev-api`
	cd web && NEXT_PUBLIC_API_URL=http://localhost:$(API_PORT) pnpm dev --port $(WEB_PORT)

web/node_modules: web/package.json web/pnpm-lock.yaml
	cd web && pnpm install --frozen-lockfile
	@touch web/node_modules

web-install: web/node_modules ## Install web dependencies

build: web/node_modules ## Build the static UI (web/out) and the release binary
	cd web && pnpm build
	cargo build --release -p chrysopoeia-server

run: build ## Build, then serve UI + API from the release binary on :8080
	./target/release/chrysopoeia --port $(API_PORT) --data-dir $(DATA_DIR) --web-dir web/out

test: web/node_modules ## Run the Rust tests (those needing ffmpeg skip without it) and the web unit tests
	cargo test --workspace
	cd web && pnpm test

lint: web/node_modules ## Check formatting, clippy, eslint, TypeScript and shell scripts
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cd web && pnpm lint && pnpm typecheck
	@if command -v shellcheck >/dev/null 2>&1; then shellcheck docker/*.sh scripts/*.sh; else echo "shellcheck not installed; skipping the shell script check"; fi

fmt: ## Format Rust code
	cargo fmt --all

docker: ## Build the Docker image (IMAGE=chrysopoeia:dev, PLATFORM=linux/amd64)
	docker build --platform $(PLATFORM) -t $(IMAGE) .

test-docker: docker ## Build the image, then test its entrypoint (PUID/PGID, GPU groups)
	docker/test-entrypoint.sh $(IMAGE)

e2e: docker ## Build the image, then run the end-to-end smoke test against it
	scripts/e2e-smoke.sh $(IMAGE)

test-media: ## Generate a small synthetic media library in ./media (MEDIA_DIR=...)
	scripts/make-test-media.sh $(MEDIA_DIR)

clean: ## Remove the web build output (keeps Rust's target/ cache)
	rm -rf web/.next web/out
