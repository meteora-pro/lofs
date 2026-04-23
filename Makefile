# LOFS dev / test / bench helpers.
#
# Two registries run side-by-side so every test suite + benchmark can cover
# the Zot / Distribution compatibility matrix:
#
#     Zot           → http://localhost:5100
#     Distribution  → http://localhost:5101

SHELL := bash
COMPOSE := docker compose -f docker/docker-compose.yml

.PHONY: dev-up dev-down dev-logs dev-status dev-reset \
        test test-unit test-e2e bench fmt clippy check \
        registry-ping zot-catalog distribution-catalog \
        docker-build-cli docker-run-cli docker-test-cli \
        help

help: ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z0-9_-]+:.*?## / {printf "  %-22s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

dev-up: ## Start Zot + Distribution (background)
	$(COMPOSE) up -d
	@echo "waiting for registries to become healthy…"
	@for i in $$(seq 1 30); do \
	    if curl -fsS http://localhost:5100/v2/ >/dev/null 2>&1 && \
	       curl -fsS http://localhost:5101/v2/ >/dev/null 2>&1; then \
	        echo "both registries ready"; \
	        echo "  zot          → http://localhost:5100"; \
	        echo "  distribution → http://localhost:5101"; \
	        exit 0; \
	    fi; \
	    sleep 1; \
	done; \
	echo "registries did not become healthy in 30s"; $(COMPOSE) ps; exit 1

dev-down: ## Stop + remove the registries
	$(COMPOSE) down

dev-reset: ## Drop all registry data and restart clean
	$(COMPOSE) down -v
	$(COMPOSE) up -d

dev-logs: ## Tail logs from both registries
	$(COMPOSE) logs -f

dev-status: ## Show registry container status
	$(COMPOSE) ps

registry-ping: ## Smoke-check both /v2/ endpoints
	@echo "--- zot"
	@curl -fsS http://localhost:5100/v2/ && echo || echo "zot unreachable"
	@echo "--- distribution"
	@curl -fsS http://localhost:5101/v2/ && echo || echo "distribution unreachable"

zot-catalog: ## Dump Zot repository catalog
	@curl -fsS http://localhost:5100/v2/_catalog | jq .

distribution-catalog: ## Dump Distribution repository catalog
	@curl -fsS http://localhost:5101/v2/_catalog | jq .

check: ## cargo check (all crates)
	cargo check --workspace --all-targets

fmt: ## cargo fmt
	cargo fmt --all

clippy: ## cargo clippy -D warnings
	cargo clippy --workspace --all-targets -- -D warnings

test: test-unit ## Alias: test-unit (e2e needs `make dev-up` first)

test-unit: ## Run unit tests (fast, no registry required)
	cargo test --workspace --lib

test-e2e: ## Run registry-backed integration tests (requires `make dev-up`)
	LOFS_TEST_ZOT=http://localhost:5100 \
	LOFS_TEST_DISTRIBUTION=http://localhost:5101 \
	cargo test --workspace --tests -- --include-ignored

bench: ## Run criterion benchmarks (requires `make dev-up`)
	LOFS_BENCH_ZOT=http://localhost:5100 \
	LOFS_BENCH_DISTRIBUTION=http://localhost:5101 \
	cargo bench --workspace

docker-build-cli: ## Build the lofs CLI as a Linux Docker image
	$(COMPOSE) --profile tools build cli

docker-run-cli: docker-build-cli ## Run a one-shot `lofs doctor` inside the compose network
	$(COMPOSE) --profile tools run --rm cli doctor

docker-test-cli: docker-build-cli ## End-to-end smoke: Linux `lofs` binary against `zot` inside the compose network
	@echo "=== Linux-binary smoke test ==="
	$(COMPOSE) --profile tools run --rm cli doctor
	@echo
	$(COMPOSE) --profile tools run --rm cli create docker-demo --ttl-days 5 --size-limit-mb 128
	@echo
	$(COMPOSE) --profile tools run --rm cli list
	@echo
	$(COMPOSE) --profile tools run --rm cli stat docker-demo
	@echo
	$(COMPOSE) --profile tools run --rm cli rm docker-demo --force
	@echo
	$(COMPOSE) --profile tools run --rm cli list
