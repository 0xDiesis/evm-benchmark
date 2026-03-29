.PHONY: build test clippy fmt check help \
       bench bench-all compare compare-all \
       sweep sweep-all sweep-list \
       diesis-up diesis-down diesis-restart diesis-status diesis-quick diesis-full diesis-tune \
       results results-latest results-compare results-summary \
       chains

CARGO  := cargo
HARNESS_MANIFEST := crates/evm-benchmark/Cargo.toml
SCRIPTS := bench-targets/scripts

# ── Defaults (override on command line) ──────────────────────────────────────
CHAIN      ?= diesis
MODE       ?= burst
ENV        ?= clean
TXS        ?= 10000
TPS        ?= 200
DURATION   ?= 30
SENDERS    ?= 200
BATCH_SIZE ?= 200
WORKERS    ?= 8
TAG        ?=
TEST_MODE  ?= transfer
CHAINS     ?= diesis sonic
PARAM      ?=
FILTER_CHAIN ?=
FILTER_MODE  ?=

# ══════════════════════════════════════════════════════════════════════════════
# Build & Quality
# ══════════════════════════════════════════════════════════════════════════════

build: ## Build the benchmark harness (release)
	$(CARGO) build --release --manifest-path $(HARNESS_MANIFEST)

build-debug: ## Build the benchmark harness (debug)
	$(CARGO) build --manifest-path $(HARNESS_MANIFEST)

test: ## Run harness unit tests
	$(CARGO) test --manifest-path $(HARNESS_MANIFEST)

clippy: ## Run clippy
	$(CARGO) clippy --manifest-path $(HARNESS_MANIFEST) -- -D warnings

fmt: ## Format all Rust code
	$(CARGO) fmt --all

check: ## Run fmt check, clippy, and tests
	$(CARGO) fmt --all --check
	$(MAKE) clippy
	$(MAKE) test

# ══════════════════════════════════════════════════════════════════════════════
# Single-Chain Benchmarks
# ══════════════════════════════════════════════════════════════════════════════

bench: build ## Run a single benchmark  (CHAIN= MODE= ENV= TXS= TPS= DURATION= SENDERS= REBUILD= DEV=)
	@bash $(SCRIPTS)/bench.sh \
		--chain $(CHAIN) --mode $(MODE) --env $(ENV) \
		--txs $(TXS) --tps $(TPS) --duration $(DURATION) \
		--senders $(SENDERS) --batch-size $(BATCH_SIZE) --workers $(WORKERS) \
		--test-mode $(TEST_MODE) \
		$(if $(TAG),--tag $(TAG)) \
		$(if $(filter true yes 1,$(REBUILD)),--rebuild) \
		$(if $(filter true yes 1,$(DEV)),--dev)

bench-all: build ## Run burst + sustained + ceiling for one chain  (CHAIN= ENV=)
	@echo "═══ Running full benchmark suite for $(CHAIN) ═══"
	@$(MAKE) --no-print-directory bench CHAIN=$(CHAIN) MODE=burst    ENV=$(ENV) TXS=$(TXS) SENDERS=$(SENDERS)
	@$(MAKE) --no-print-directory bench CHAIN=$(CHAIN) MODE=sustained ENV=$(ENV) TPS=$(TPS) DURATION=$(DURATION) SENDERS=$(SENDERS)
	@$(MAKE) --no-print-directory bench CHAIN=$(CHAIN) MODE=ceiling  ENV=$(ENV) TPS=$(TPS) SENDERS=$(SENDERS)

# ══════════════════════════════════════════════════════════════════════════════
# Head-to-Head Comparisons
# ══════════════════════════════════════════════════════════════════════════════

compare: build ## Compare chains  (CHAINS="diesis sonic" MODE= ENV=)
	@bash $(SCRIPTS)/compare.sh \
		--chains "$(CHAINS)" --mode $(MODE) --env $(ENV) \
		--txs $(TXS) --tps $(TPS) --duration $(DURATION) \
		--senders $(SENDERS) --batch-size $(BATCH_SIZE)

compare-all: build ## Compare chains across all modes  (CHAINS="diesis sonic" ENV=)
	@bash $(SCRIPTS)/compare.sh \
		--chains "$(CHAINS)" --mode all --env $(ENV) \
		--txs $(TXS) --tps $(TPS) --duration $(DURATION) \
		--senders $(SENDERS) --batch-size $(BATCH_SIZE)

# ══════════════════════════════════════════════════════════════════════════════
# Parameter Sweeps (Diesis-specific)
# ══════════════════════════════════════════════════════════════════════════════

sweep: build ## Run a parameter sweep  (PARAM=block-period MODE= TXS=)
	@test -n "$(PARAM)" || { echo "Error: PARAM is required.  Run 'make sweep-list' to see options."; exit 2; }
	@bash $(SCRIPTS)/sweep.sh \
		--param $(PARAM) --mode $(MODE) \
		--txs $(TXS) --tps $(TPS) --senders $(SENDERS) --batch-size $(BATCH_SIZE)

sweep-all: build ## Run all parameter sweeps sequentially
	@for p in block-period ordering-window max-block-txs parallel-execution commitment-mode proposal-gas round-delay max-proposal-txs; do \
		echo ""; echo "═══ Sweep: $$p ═══"; \
		bash $(SCRIPTS)/sweep.sh --param $$p --mode $(MODE) --txs $(TXS) --tps $(TPS) --senders $(SENDERS) --batch-size $(BATCH_SIZE); \
	done

sweep-list: ## List available sweep profiles
	@bash $(SCRIPTS)/sweep.sh --list

# ══════════════════════════════════════════════════════════════════════════════
# Diesis Integration (shortcuts)
# ══════════════════════════════════════════════════════════════════════════════

DIESIS_REPO ?= $(abspath $(CURDIR)/../diesis)

diesis-up: ## Start Diesis e2e cluster (release build)
	$(MAKE) -C "$(DIESIS_REPO)" e2e-up-release

diesis-down: ## Stop Diesis e2e cluster
	$(MAKE) -C "$(DIESIS_REPO)" e2e-down

diesis-restart: diesis-down diesis-up ## Clean restart Diesis cluster

diesis-status: ## Check block heights on all Diesis validators
	@for port in 8545 8555 8565 8575; do \
		height=$$(curl -sf http://localhost:$$port \
			-X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "N/A"); \
		echo "  :$$port -> block $$height"; \
	done

diesis-quick: build ## Quick Diesis burst benchmark (500 txs, no clean restart)
	@$(MAKE) --no-print-directory bench CHAIN=diesis MODE=burst ENV=clean TXS=500 SENDERS=100 TAG=quick

diesis-full: build ## Full Diesis benchmark suite (burst + sustained + ceiling)
	@$(MAKE) --no-print-directory bench-all CHAIN=diesis ENV=clean

diesis-tune: build ## Run all parameter sweeps against Diesis
	@$(MAKE) --no-print-directory sweep-all

diesis-geo: build ## Run Diesis benchmarks across all geo-latency profiles
	@for geo in geo-global geo-us geo-eu geo-degraded geo-intercontinental; do \
		echo ""; echo "═══ Diesis burst — $$geo ═══"; \
		$(MAKE) --no-print-directory bench CHAIN=diesis MODE=burst ENV=$$geo TXS=$(TXS) SENDERS=$(SENDERS); \
	done

# ══════════════════════════════════════════════════════════════════════════════
# Results Management
# ══════════════════════════════════════════════════════════════════════════════

results: ## List all benchmark results  (FILTER_CHAIN= FILTER_MODE= — optional filters)
	@bash $(SCRIPTS)/results.sh list \
		$(if $(FILTER_CHAIN),--chain $(FILTER_CHAIN)) \
		$(if $(FILTER_MODE),--mode $(FILTER_MODE))

results-latest: ## Show the latest benchmark result  (FILTER_CHAIN= FILTER_MODE= — optional filters)
	@bash $(SCRIPTS)/results.sh latest \
		$(if $(FILTER_CHAIN),--chain $(FILTER_CHAIN)) \
		$(if $(FILTER_MODE),--mode $(FILTER_MODE))

results-compare: ## Ad-hoc compare runs  (RUNS="path1 path2")
	@test -n "$(RUNS)" || { echo "Error: RUNS is required, e.g. RUNS='results/runs/diesis/burst/... results/runs/sonic/burst/...'"; exit 2; }
	@bash $(SCRIPTS)/results.sh compare $(RUNS)

results-summary: ## Aggregate stats across all runs
	@bash $(SCRIPTS)/results.sh summary

# ══════════════════════════════════════════════════════════════════════════════
# Info
# ══════════════════════════════════════════════════════════════════════════════

chains: ## List all registered chains
	@bash -c 'source $(SCRIPTS)/lib.sh && list_chains'

help: ## Show this help
	@echo ""
	@echo "  Benchmark Suite — Diesis Performance Testing"
	@echo "  ─────────────────────────────────────────────"
	@echo ""
	@echo "  Quick start:"
	@echo "    make diesis-up              # Start Diesis e2e cluster"
	@echo "    make diesis-quick           # Quick 500-tx burst test"
	@echo "    make bench                  # Full 10000-tx burst test"
	@echo "    make bench MODE=ceiling     # Find max throughput"
	@echo "    make compare                # Diesis vs Sonic head-to-head"
	@echo "    make results                # List all results"
	@echo ""
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "  Variables:"
	@echo "    CHAIN=$(CHAIN)  MODE=$(MODE)  ENV=$(ENV)  TXS=$(TXS)  TPS=$(TPS)"
	@echo "    DURATION=$(DURATION)  SENDERS=$(SENDERS)  BATCH_SIZE=$(BATCH_SIZE)"
	@echo "    CHAINS=\"$(CHAINS)\"  PARAM=$(PARAM)"
	@echo "    FILTER_CHAIN=$(FILTER_CHAIN)  FILTER_MODE=$(FILTER_MODE)  (for results/results-latest)"
