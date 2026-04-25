.PHONY: up down restart bench bench-sustained bench-ceiling status

SCRIPT_DIR := $(shell cd "$(dir $(lastword $(MAKEFILE_LIST)))" && pwd)
BENCH_REPO_DIR := $(shell cd "$(SCRIPT_DIR)/../../.." && pwd)
DIESIS_REPO_DIR ?= $(abspath $(BENCH_REPO_DIR)/../diesis)

up: ## Start the Diesis e2e cluster (requires the Diesis source repo at $(DIESIS_REPO_DIR))
	@if [ ! -d "$(DIESIS_REPO_DIR)" ]; then \
	  echo "ERROR: Diesis source repo not found at $(DIESIS_REPO_DIR)."; \
	  echo "       Set DIESIS_REPO_DIR to a checkout of the Diesis repository,"; \
	  echo "       or bring up your own Diesis cluster manually before running benchmarks."; \
	  exit 2; \
	fi
	$(MAKE) -C "$(DIESIS_REPO_DIR)" e2e-up-release

down: ## Stop the Diesis e2e cluster (requires the Diesis source repo at $(DIESIS_REPO_DIR))
	@if [ ! -d "$(DIESIS_REPO_DIR)" ]; then \
	  echo "ERROR: Diesis source repo not found at $(DIESIS_REPO_DIR)."; \
	  exit 2; \
	fi
	$(MAKE) -C "$(DIESIS_REPO_DIR)" e2e-down

restart: down up ## Restart the Diesis e2e cluster

bench: ## Run the default burst benchmark against Diesis
	@bash ./run-bench.sh

bench-sustained: ## Run a sustained benchmark against Diesis
	@bash ./run-bench.sh --execution sustained --tps 500 --duration 30

bench-ceiling: ## Run a ceiling benchmark against Diesis
	@bash ./run-bench.sh --execution ceiling --tps 200

status: ## Check block heights on the Diesis e2e validators
	@for port in 8545 8555 8565 8575; do \
		height=$$(curl -sf http://localhost:$$port \
			-X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "N/A"); \
		echo "  :$$port -> block $$height"; \
	done
