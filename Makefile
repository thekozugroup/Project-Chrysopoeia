.PHONY: dev build docker docker-up docker-down check clean lint test help

# Default target
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

dev: ## Start web dev server
	cd web && pnpm dev

build: ## Build web frontend and Rust backend
	cd web && pnpm install --frozen-lockfile && pnpm build
	cargo build --release

docker: ## Build Docker image
	docker build -t chrysopoeia:latest .

docker-up: ## Start services with docker compose
	docker compose up -d

docker-down: ## Stop services with docker compose
	docker compose down

check: ## Run cargo check, clippy, and pnpm lint
	cargo check --workspace
	cargo clippy --workspace -- -D warnings
	cd web && pnpm install --frozen-lockfile && pnpm lint

lint: ## Run linters only (clippy + eslint)
	cargo clippy --workspace -- -D warnings
	cd web && pnpm lint

test: ## Run Rust tests
	cargo test --workspace

clean: ## Remove build artifacts
	cargo clean
	rm -rf web/.next web/node_modules
