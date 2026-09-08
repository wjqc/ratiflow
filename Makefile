.PHONY: help install codegen codegen-drift fmt fmt-check typecheck test test-rust test-web e2e build build-core build-desktop run package clean clippy ci rdws-manifest old-binary-drill

.DEFAULT_GOAL := help

help: ## Show available commands
	@awk 'BEGIN {FS = ":.*## "}; /^[a-zA-Z_-]+:.*## / {printf "%-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

install: ## Install JS dependencies (Electron via npmmirror)
	ELECTRON_MIRROR=https://npmmirror.com/mirrors/electron/ npm install

codegen: ## Generate TS protocol types from contracts/rpc
	node packages/protocol/generate.mjs

codegen-drift: ## codegen 重生成必须幂等（生成物被手改/契约与实现脱节即失败）
	@mkdir -p target
	@cp packages/protocol/src/generated.ts target/gen-before-protocol.ts
	@cp apps/desktop/src/main/rpcMethods.generated.ts target/gen-before-rpcmethods.ts
	node packages/protocol/generate.mjs
	@cmp -s packages/protocol/src/generated.ts target/gen-before-protocol.ts || \
		{ echo "codegen drift: generated.ts 与既有生成物不一致（契约先行被破坏或生成物被手改）"; exit 1; }
	@cmp -s apps/desktop/src/main/rpcMethods.generated.ts target/gen-before-rpcmethods.ts || \
		{ echo "codegen drift: rpcMethods.generated.ts 与既有生成物不一致"; exit 1; }
	@echo "codegen 幂等检查通过" 

typecheck: ## TypeScript typecheck (renderer + main + preload + protocol)
	npm run typecheck

fmt: ## cargo fmt (rewrite)
	cargo fmt --all

fmt-check: ## cargo fmt --check (CI gate, no rewrite)
	cargo fmt --all -- --check

clippy: ## cargo clippy (warnings as errors)
	cargo clippy --all-targets -- -D warnings

test-rust: ## Rust test suite
	cargo test

test-web: ## Renderer component tests
	npm run test

test: test-rust test-web ## All unit tests

test-electron: ## Electron E2E scenarios A/D/E/F (Playwright)
	npm --workspace @ratiflow/desktop run build
	cd apps/desktop && npx playwright test

UNAME_S := $(shell uname -s)

e2e: ## Protocol E2E（平台无关面 + 平台清单：按宿主平台选 MCP 成功链/负例，非目标平台显式声明跳过原因）
	cargo build --release -p ratiflow-core
	node tests/e2e-protocol/with-timeout.mjs 300 e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 agent-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 settings-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 trace-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 gate-release-race-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 rollback-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 agent-routing-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 memory-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 workflow-template-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 gate-acceptance-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 release-notes-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 gate-skip-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 rework-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 metrics-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 workitem-search-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 rpc-receipt-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 knowledge-verification-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 risk-ledger-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 mcp-import-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 plan-dag-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 plan-execution-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 partial-replan-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 team-context-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 trace-command-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 automation-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 dirty-main-probe-e2e.mjs
	node tests/e2e-protocol/with-timeout.mjs 300 e1-repro.mjs
	@echo "---- PLATFORM MANIFEST ($(UNAME_S)) ----"
ifeq ($(UNAME_S),Darwin)
	node tests/e2e-protocol/with-timeout.mjs 300 mcp-e2e.mjs
	@echo "mcp-macos-success: 本机执行 MCP 成功链（mcp-e2e + Seatbelt）"
	@echo "mcp-linux-unsupported: 仅 Linux 负例 → 本机跳过，由 CI job mcp-linux-unsupported 覆盖"
	@echo "windows-product-negative: 仅 Windows 负例 → 本机跳过，由 CI job windows-product-negative 覆盖"
else ifeq ($(UNAME_S),Linux)
	node tests/e2e-protocol/with-timeout.mjs 300 mcp-platform-e2e.mjs
	@echo "mcp-linux-unsupported: 本机执行 Linux fail-loud 负例（mcp-platform-e2e）"
	@echo "mcp-macos-success: 仅 macOS 成功链 → 本机跳过，由 CI job mcp-macos-success 覆盖"
	@echo "windows-product-negative: 仅 Windows 负例 → 本机跳过，由 CI job windows-product-negative 覆盖"
else
	@echo "mcp-linux-unsupported / mcp-macos-success / windows-product-negative: 宿主平台均非目标 → 全部由专用 CI job 覆盖"
endif
	@echo "migration-old-binary-drill: 重型演练（构建旧 core），不在 make ci 内——手动 make old-binary-drill 或 CI 专用 job"
	@echo "---- END PLATFORM MANIFEST ----"

old-binary-drill: ## 旧二进制演练（兼容 + 0049-era 拒启 + 快照恢复；重型：构建旧 core）
	cargo build --release -p ratiflow-core
	bash tests/platform/old-binary-drill.sh

rdws-manifest: ## RDWS-001~020 证据矩阵校验（空证据/重复 ID/未进 CI 脚本即失败）
	node tests/rdws/validate.mjs

release-diagnostics: ## R0 安全封锁诊断与证据清单（§10.1：flag 负例探针 + 只读计数器 JSON）
	cargo build --release -p ratiflow-core
	node tests/platform/release-diagnostics.mjs

test-contract: ## Contract fixtures + decoders + secret probe (F11/M4)
	node tests/contract/run.mjs

build-core: ## Release build of Rust core
	cargo build --release -p ratiflow-core

build-desktop: ## Build Electron main/preload/renderer
	npm --workspace @ratiflow/desktop run build

build: build-core build-desktop ## Build everything

run: build ## Launch desktop app
	npm --workspace @ratiflow/desktop run start

package: build ## Package desktop app (dir, unsigned)
	npm --workspace @ratiflow/desktop run package

ci: fmt-check clippy codegen-drift typecheck test test-contract rdws-manifest e2e test-electron ## Local CI sequence（平台清单见 e2e 目标输出）

clean: ## Clean build outputs
	cargo clean
	rm -rf apps/desktop/dist
