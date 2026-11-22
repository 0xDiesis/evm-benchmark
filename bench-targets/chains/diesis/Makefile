.PHONY: up down restart image bench bench-sustained bench-ceiling status

SCRIPT_DIR := $(shell cd "$(dir $(lastword $(MAKEFILE_LIST)))" && pwd)
BENCH_REPO_DIR := $(shell cd "$(SCRIPT_DIR)/../../.." && pwd)
DIESIS_REPO_DIR ?= $(abspath $(BENCH_REPO_DIR)/../diesis)

FLEET := python3 "$(SCRIPT_DIR)/fleet.py"
export DIESIS_REPO_DIR

up: ## Start a fresh Diesis fleet from an existing image (DIESIS_IMAGE_TAG, BENCH_DIESIS_PROFILE)
	$(FLEET) reset

down: ## Remove the fresh fleet (an aged fleet, DIESIS_AGED_TAG, is only stopped)
	$(FLEET) down

restart: ## Restart the fleet from a fresh genesis
	$(FLEET) reset

image: ## Build diesis-node-e2e:<HEAD>-release from a clean node checkout (run through the compute broker)
	bash "$(SCRIPT_DIR)/build-image.sh" "$(DIESIS_REPO_DIR)"

bench: ## Run the default burst benchmark against Diesis
	@bash ./run-bench.sh

bench-sustained: ## Run a sustained benchmark against Diesis
	@bash ./run-bench.sh --execution sustained --tps 500 --duration 30

bench-ceiling: ## Run a ceiling benchmark against Diesis
	@bash ./run-bench.sh --execution ceiling --tps 200

status: ## Check block heights on the Diesis e2e validators
	@for port in 18545 18555 18565 18575; do \
		height=$$(curl -sf http://localhost:$$port \
			-X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "N/A"); \
		echo "  :$$port -> block $$height"; \
	done
