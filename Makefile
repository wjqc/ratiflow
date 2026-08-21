.PHONY: help install build build-web build-extension build-go test test-web test-go typecheck ci run clean

.DEFAULT_GOAL := help

help: ## Show available commands
	@awk 'BEGIN {FS = ":.*## "}; /^[a-zA-Z_-]+:.*## / {printf "%-20s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

install: ## Install JavaScript dependencies
	npm install

build-web: ## Build React into Go embedded assets
	npm run build:web

build-extension: ## Compile the VS Code extension
	npm run build:extension

build-go: ## Build the local Go binary (requires Go 1.26+)
	go build -trimpath -ldflags "-X main.version=0.1.0" -o dist/sixgates ./cmd/sixgates

build: build-web build-extension build-go ## Build all deliverables

test-web: ## Run frontend unit tests
	npm run test:web

test-go: ## Run isolated Go tests
	go test ./...

test: test-web test-go ## Run all tests

typecheck: ## Type-check TypeScript workspaces
	npm run typecheck

ci: typecheck test build ## Run the local CI sequence

run: build-web ## Run the local service
	go run ./cmd/sixgates --address 127.0.0.1:7666 --data-dir ./data

clean: ## Remove generated build output, preserving source and local data
	find apps -type d -name dist -prune -exec rm -r {} +
	find internal/web/dist -mindepth 1 ! -name index.html -delete
	rm -r dist 2>/dev/null || true
