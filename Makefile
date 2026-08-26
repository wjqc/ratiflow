.PHONY: help install codegen typecheck test test-rust test-web e2e build build-core build-desktop run package clean fmt clippy ci

.DEFAULT_GOAL := help

help: ## Show available commands
	@awk 'BEGIN {FS = ":.*## "}; /^[a-zA-Z_-]+:.*## / {printf "%-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

install: ## Install JS dependencies (Electron via npmmirror)
	ELECTRON_MIRROR=https://npmmirror.com/mirrors/electron/ npm install

codegen: ## Generate TS protocol types from contracts/rpc
	node packages/protocol/generate.mjs

typecheck: ## TypeScript typecheck (renderer + main + preload + protocol)
	npm run typecheck

fmt: ## cargo fmt
	cargo fmt --all

clippy: ## cargo clippy (warnings as errors)
	cargo clippy --all-targets -- -D warnings

test-rust: ## Rust test suite
	cargo test

test-web: ## Renderer component tests
	npm run test

test: test-rust test-web ## All unit tests

test-electron: ## Electron E2E scenarios A/D/E/F (Playwright)
	npm --workspace @sixgates/desktop run build
	cd apps/desktop && npx playwright test

e2e: ## Protocol E2E golden flow + agent lifecycle + settings (requires release core build)
	cargo build --release -p sixgates-core
	node tests/e2e-protocol/e2e.mjs
	node tests/e2e-protocol/agent-e2e.mjs
	node tests/e2e-protocol/settings-e2e.mjs

test-contract: ## Contract fixtures + decoders + secret probe (F11/M4)
	node tests/contract/run.mjs

build-core: ## Release build of Rust core
	cargo build --release -p sixgates-core

build-desktop: ## Build Electron main/preload/renderer
	npm --workspace @sixgates/desktop run build

build: build-core build-desktop ## Build everything

run: build ## Launch desktop app
	npm --workspace @sixgates/desktop run start

package: build ## Package desktop app (dir, unsigned)
	npm --workspace @sixgates/desktop run package

ci: fmt clippy codegen typecheck test test-contract e2e test-electron ## Local CI sequence

clean: ## Clean build outputs
	cargo clean
	rm -rf apps/desktop/dist
