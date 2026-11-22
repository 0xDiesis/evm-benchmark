.PHONY: build test test-scripts clippy fmt check quality quality-fix clean-rust help preflight \
       bench bench-all compare compare-all ab diesis-age \
       sweep sweep-all sweep-list \
       results results-latest results-compare results-ledger results-summary \
       chains \
       bytecode bytecode-check

CARGO  := cargo
export RUSTC_WRAPPER ?= $(if $(filter 1,$(DIESIS_USE_SCCACHE)),sccache,)
FORGE := $(CURDIR)/scripts/forge.sh
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
WORKERS    ?=
TAG        ?=
TEST_MODE  ?= transfer
CHAINS     ?= diesis sonic
PARAM      ?=
FILTER_CHAIN ?=
FILTER_MODE  ?=

# Diesis: FLEET=external benchmarks an already-running fleet at DIESIS_RPC / DIESIS_WS
# without starting, stopping or restarting it; CHAIN_ID= overrides eth_chainId.
ifdef FLEET
export DIESIS_FLEET := $(FLEET)
endif
ifdef CHAIN_ID
export DIESIS_CHAIN_ID := $(CHAIN_ID)
endif
# Diesis fleet (docs/diesis-methodology.md): PROFILE=capped|uncapped (capped is the official
# run rule), IMAGE=<diesis-node-e2e tag> (default <checkout HEAD>-release), AGED=<aged fleet
# tag>, WARMUP=<seconds excluded from the measured window>.
ifdef PROFILE
export BENCH_DIESIS_PROFILE := $(PROFILE)
endif
ifdef IMAGE
export DIESIS_IMAGE_TAG := $(IMAGE)
endif
ifdef AGED
export DIESIS_AGED_TAG := $(AGED)
endif
ifdef WARMUP
export BENCH_WARMUP_SECS := $(WARMUP)
endif

# ══════════════════════════════════════════════════════════════════════════════
# Build & Quality
# ══════════════════════════════════════════════════════════════════════════════

build: ## Build the benchmark harness (release)
	$(CARGO) build --release --manifest-path $(HARNESS_MANIFEST)

build-debug: ## Build the benchmark harness (debug)
	$(CARGO) build --manifest-path $(HARNESS_MANIFEST)

test: ## Run harness unit tests
	$(CARGO) test --manifest-path $(HARNESS_MANIFEST)

test-scripts: ## Run local launcher and diagnostics tests (not part of CI)
	@bash $(SCRIPTS)/test-cargo-env.sh
	@python3 scripts/tests/test_forge_launcher.py
	@python3 scripts/tests/test_joltzu_target.py
	@python3 scripts/tests/test_latency_proxy.py
	@python3 $(SCRIPTS)/test_prometheus_diagnostics.py
	@python3 -m unittest discover -s scripts/tests -p 'test_diesis_*.py'

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

clean-rust: ## Remove generated Rust build artifacts
	rm -rf target

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
	cd $(CONTRACTS_DIR) && "$(FORGE)" build
	@for name in $(BYTECODE_NAMES); do \
	  jq -r '.bytecode.object' $(CONTRACTS_DIR)/out/$$name.sol/$$name.json \
	    | sed 's/^0x//' | tr -d '\n' > $(BYTECODE_DIR)/$$name.hex; \
	  echo "regenerated $(BYTECODE_DIR)/$$name.hex"; \
	done

bytecode-check: ## Fail if committed .hex files drifted from contracts/ source
	@cd $(CONTRACTS_DIR) && "$(FORGE)" build > /dev/null
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

bench: build ## Run a single benchmark  (CHAIN= MODE= ENV= TXS= TPS= DURATION= SENDERS= REBUILD= DEV= MIX=judged pins its own senders)
	@bash $(SCRIPTS)/bench.sh \
		--chain $(CHAIN) --mode $(MODE) --env $(ENV) \
		--txs $(TXS) --tps $(TPS) --duration $(DURATION) \
		$(if $(filter judged,$(MIX)),--mix judged,--senders $(SENDERS)) --batch-size $(BATCH_SIZE) $(if $(WORKERS),--workers $(WORKERS)) \
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
# Diesis A/B and aged fleets (docs/diesis-methodology.md)
# ══════════════════════════════════════════════════════════════════════════════

ab: build ## Interleaved A/B/B/A Diesis runs  (NAME= A= B= ARMS=3 RATES=2000 AB_DURATION=660 WARMUP=60 AGED= PROBE=tps:s REUSE_FLEET=1 MAX_LOAD=)
	@test -n "$(NAME)" || { echo "Error: NAME is required"; exit 2; }
	@python3 $(SCRIPTS)/ab.py --name "$(NAME)" --a "$(or $(A),profile=capped)" $(if $(B),--b "$(B)") \
		--arms $(or $(ARMS),3) --rates "$(or $(RATES),2000)" --duration $(or $(AB_DURATION),660) \
		--warmup $(or $(WARMUP),60) $(if $(AGED),--aged $(AGED)) $(if $(PROBE),--probe $(PROBE)) \
		$(if $(filter 1 true yes,$(REUSE_FLEET)),--reuse-fleet) $(if $(MAX_LOAD),--max-load $(MAX_LOAD)) $(if $(MAX_RERUNS),--max-reruns $(MAX_RERUNS))

diesis-judged: build ## The judged run  (FIXTURE=<saved fixture> or TAG=<aged fleet>, RATE=, OUTCOME=bls-only|hybrid, MIX=judged|transfer PROFILE=capped WINDOW=600 MAX_LOAD=18)
	@test -n "$(RATE)" -a -n "$(FIXTURE)$(TAG)" -a -n "$(OUTCOME)" || { echo "Error: RATE, OUTCOME and FIXTURE (or TAG) are required"; exit 2; }
	@python3 $(SCRIPTS)/judged.py $(if $(FIXTURE),--fixture "$(FIXTURE)",--tag "$(TAG)") --rate $(RATE) \
		--outcome-rule $(OUTCOME) --mix $(or $(MIX),judged) \
		--profile $(or $(PROFILE),capped) --window $(or $(WINDOW),600) --max-load $(or $(MAX_LOAD),18)

diesis-fixture-save: ## Archive a stopped aged fleet as a fixture  (TAG=)
	@python3 bench-targets/chains/diesis/fleet.py save-fixture --tag "$(TAG)"

diesis-fixture-restore: ## Restore a fixture into a new aged fleet  (FIXTURE= AS=)
	@python3 bench-targets/chains/diesis/fleet.py restore-fixture --fixture "$(FIXTURE)" --as "$(AS)"

# The node's built speed flags (keys and ranges as at main 0016cf806 and 9ba6562b2), all off
# against all on. Booleans take only true/false; the two selected warm-preparation flags are 0/1
# integers; pipeline depth is 1..3.
# Excluded: DIESIS_SELECTED_APPEND_DURABLE_DUPLICATE_REPLAY (rejected) and
# DIESIS_MPT_SERVE_EXECUTED_HEAD (stays off).
SPEED_FLAGS_OFF := DIESIS_HORIZON_APPEND_BEFORE_RETIRE=false+DIESIS_MPT_SINGLE_AUTHORITY_CHECK=false+DIESIS_MPT_EARLY_VOTES=false+DIESIS_MPT_PIPELINE_DEPTH=1+DIESIS_SELECTED_AGREEMENT_CAPTURE_OUTSIDE_OWNER=false+DIESIS_SELECTED_WARM_PUBLICATION_REFS=0+DIESIS_SELECTED_QUEUE_DIGEST_PER_REVISION=0
SPEED_FLAGS_ON  := DIESIS_HORIZON_APPEND_BEFORE_RETIRE=true+DIESIS_MPT_SINGLE_AUTHORITY_CHECK=true+DIESIS_MPT_EARLY_VOTES=true+DIESIS_MPT_PIPELINE_DEPTH=2+DIESIS_SELECTED_AGREEMENT_CAPTURE_OUTSIDE_OWNER=true+DIESIS_SELECTED_WARM_PUBLICATION_REFS=1+DIESIS_SELECTED_QUEUE_DIGEST_PER_REVISION=1

diesis-speed-flags: build ## Speed flags off vs on, interleaved A B B A  (NAME= FIXTURE= or AGED= or fresh, RATES=2000 ARMS=3 AB_DURATION=660 WARMUP=60 PROFILE=capped EXTRA=K=V+K=V on both sides, LABEL= side-name suffix, NOTE= summary line)
	@test -n "$(NAME)" || { echo "Error: NAME is required"; exit 2; }
	@python3 $(SCRIPTS)/ab.py --name "$(NAME)" $(if $(NOTE),--note "$(NOTE)") \
		--a "profile=$(or $(PROFILE),capped),env=$(SPEED_FLAGS_OFF)$(if $(EXTRA),+$(EXTRA)),name=flags-off$(if $(LABEL),-$(LABEL))" \
		--b "profile=$(or $(PROFILE),capped),env=$(SPEED_FLAGS_ON)$(if $(EXTRA),+$(EXTRA)),name=flags-on$(if $(LABEL),-$(LABEL))" \
		--arms $(or $(ARMS),3) --rates "$(or $(RATES),2000)" --duration $(or $(AB_DURATION),660) \
		--warmup $(or $(WARMUP),60) $(if $(FIXTURE),--fixture $(FIXTURE)) $(if $(AGED),--aged $(AGED)) \
		$(if $(MAX_LOAD),--max-load $(MAX_LOAD)) $(if $(filter 1 true yes,$(NO_QUIET_WAIT)),--no-quiet-wait)

# A comma inside $(or ...) splits its arguments, so the default stretch list is a variable.
IDLE_STRETCH_DEFAULT := 4,1

diesis-idle: ## Idle CPU, cadence and resume latency per DIESIS_IDLE_STRETCH, interleaved  (STRETCH=4,1 ARMS=1 PROFILE=capped IDLE=300 PROBES=5 PROBE_IDLE=120)
	@python3 $(SCRIPTS)/diesis-idle.py --stretch "$(or $(STRETCH),$(IDLE_STRETCH_DEFAULT))" --arms $(or $(ARMS),1) \
		--profile $(or $(PROFILE),capped) --idle $(or $(IDLE),300) --probes $(or $(PROBES),5) \
		--probe-idle $(or $(PROBE_IDLE),120)

diesis-age: build ## Build an aged Diesis fleet  (TAG= BLOCKS=5100 RATE=400)
	@test -n "$(TAG)" || { echo "Error: TAG is required"; exit 2; }
	@python3 bench-targets/chains/diesis/fleet.py age --tag "$(TAG)" --blocks $(or $(BLOCKS),5100) --rate $(or $(RATE),400)

# ══════════════════════════════════════════════════════════════════════════════
# Parameter Sweeps (Diesis-specific)
# ══════════════════════════════════════════════════════════════════════════════

sweep: build ## Run a parameter sweep  (PARAM=block-period MODE= TXS=)
	@test -n "$(PARAM)" || { echo "Error: PARAM is required.  Run 'make sweep-list' to see options."; exit 2; }
	@bash $(SCRIPTS)/sweep.sh \
		--param $(PARAM) --mode $(MODE) \
		--txs $(TXS) --tps $(TPS) --senders $(SENDERS) --batch-size $(BATCH_SIZE)

sweep-all: build ## Run all parameter sweeps sequentially
	@for p in block-period ordering-window max-block-txs parallel-execution proposal-gas round-delay max-proposal-txs; do \
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

results-ledger: ## Export run ledger with config knobs and diagnostics  (FILTER_CHAIN= FILTER_MODE= FORMAT=table|csv|md|json)
	@bash $(SCRIPTS)/results.sh ledger \
		$(if $(FILTER_CHAIN),--chain $(FILTER_CHAIN)) \
		$(if $(FILTER_MODE),--mode $(FILTER_MODE)) \
		$(if $(FORMAT),--format $(FORMAT))

results-summary: ## Aggregate stats across all runs
	@bash $(SCRIPTS)/results.sh summary

# ══════════════════════════════════════════════════════════════════════════════
# Info
# ══════════════════════════════════════════════════════════════════════════════

chains: ## List all registered chains
	@bash -c 'source $(SCRIPTS)/lib.sh && list_chains'

preflight: ## Check local benchmark tools without starting containers
	@command -v $(CARGO) >/dev/null || { echo "ERROR: cargo not found" >&2; exit 1; }
	@if [ -n "$(RUSTC_WRAPPER)" ]; then \
		command -v "$(firstword $(RUSTC_WRAPPER))" >/dev/null || { echo "ERROR: Rust compiler wrapper '$(firstword $(RUSTC_WRAPPER))' not found" >&2; exit 1; }; \
	fi
	@command -v python3 >/dev/null || { echo "ERROR: python3 not found" >&2; exit 1; }
	@command -v curl >/dev/null || { echo "ERROR: curl not found" >&2; exit 1; }
	@command -v docker >/dev/null || { echo "ERROR: docker not found" >&2; exit 1; }
	@docker compose version >/dev/null || { echo "ERROR: docker compose plugin not found" >&2; exit 1; }
	@$(CARGO) metadata --no-deps --format-version 1 --manifest-path $(HARNESS_MANIFEST) >/dev/null
	@echo "preflight: local benchmark tools available"

help: ## Show this help
	@echo ""
	@echo "  EVM Benchmark Suite"
	@echo "  ───────────────────"
	@echo ""
	@echo "  Quick start:"
	@echo "    make preflight                       # Check tools; does not start containers"
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
