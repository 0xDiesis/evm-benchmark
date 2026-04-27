.PHONY: build test clippy fmt check quality quality-fix help \
       bench bench-all compare compare-all \
       sweep sweep-all sweep-list \
       results results-latest results-compare results-summary \
       chains \
       bytecode bytecode-check

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

quality: ## Run static quality checks
	$(CARGO) fmt --all --check
	$(MAKE) clippy

quality-fix: ## Apply automatic quality fixes where available
	$(CARGO) fmt --all
	$(CARGO) clippy --fix --manifest-path $(HARNESS_MANIFEST) --allow-dirty --allow-staged -- -D warnings

# ══════════════════════════════════════════════════════════════════════════════
# Benchmark contract bytecode
# ══════════════════════════════════════════════════════════════════════════════
# The Rust harness embeds creation bytecode for BenchmarkToken/Pair/NFT via
# include_str! from crates/evm-benchmark/bytecode/*.hex. Those files are
# regenerated from contracts/src/benchmark/*.sol via `make bytecode`. CI runs
# `make bytecode-check` to fail if the committed .hex files have drifted from
# what the current sources produce.

CONTRACTS_DIR  := contracts
BYTECODE_DIR   := crates/evm-benchmark/bytecode
BYTECODE_NAMES := BenchmarkToken BenchmarkPair BenchmarkNFT

bytecode: ## Regenerate Token/Pair/NFT bytecode from contracts/ into the harness embed dir
	cd $(CONTRACTS_DIR) && forge build
	@for name in $(BYTECODE_NAMES); do \
	  jq -r '.bytecode.object' $(CONTRACTS_DIR)/out/$$name.sol/$$name.json \
	    | sed 's/^0x//' | tr -d '\n' > $(BYTECODE_DIR)/$$name.hex; \
	  echo "regenerated $(BYTECODE_DIR)/$$name.hex"; \
	done

bytecode-check: ## Fail if committed .hex files drifted from contracts/ source
	@cd $(CONTRACTS_DIR) && forge build > /dev/null
	@status=0; \
	for name in $(BYTECODE_NAMES); do \
	  fresh=$$(jq -r '.bytecode.object' $(CONTRACTS_DIR)/out/$$name.sol/$$name.json | sed 's/^0x//' | tr -d '\n'); \
	  committed=$$(tr -d '\n' < $(BYTECODE_DIR)/$$name.hex); \
	  if [ "$$fresh" != "$$committed" ]; then \
	    echo "DRIFT: $(BYTECODE_DIR)/$$name.hex does not match contracts/src/benchmark/$$name.sol"; \
	    echo "       run 'make bytecode' and commit the result"; \
	    status=1; \
	  fi; \
	done; \
	if [ $$status -eq 0 ]; then echo "bytecode-check: all .hex files in sync with contracts/"; fi; \
	exit $$status

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
	@echo "  EVM Benchmark Suite"
	@echo "  ───────────────────"
	@echo ""
	@echo "  Quick start:"
	@echo "    make chains                          # List registered chains"
	@echo "    make bench CHAIN=<chain>             # Run a 10000-tx burst against <chain>"
	@echo "    make bench CHAIN=<chain> MODE=ceiling  # Find max throughput"
	@echo "    make compare CHAINS=\"a b\"            # Head-to-head comparison"
	@echo "    make results                         # List all results"
	@echo ""
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "  Variables:"
	@echo "    CHAIN=$(CHAIN)  MODE=$(MODE)  ENV=$(ENV)  TXS=$(TXS)  TPS=$(TPS)"
	@echo "    DURATION=$(DURATION)  SENDERS=$(SENDERS)  BATCH_SIZE=$(BATCH_SIZE)"
	@echo "    CHAINS=\"$(CHAINS)\"  PARAM=$(PARAM)"
	@echo "    FILTER_CHAIN=$(FILTER_CHAIN)  FILTER_MODE=$(FILTER_MODE)  (for results/results-latest)"
