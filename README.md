# EVM Benchmark

Benchmarking and chain-comparison toolkit for EVM-compatible networks, centered on the Rust `evm-benchmark` load generator and a set of reproducible local chain targets.

This repository is chain-agnostic: benchmark orchestration, reporting, and harness behavior are shared across all supported targets.

## Prerequisites

- `docker compose` (required for chain targets)
- Rust toolchain `>= 1.93` (if building from source)
- Optional: `make`, `python3`, `curl` for orchestration scripts

## Install

### Option A: npm (easiest)

```bash
npm install -g @0xdiesis/evm-benchmark
evm-benchmark --setup
```

Or run without installing:

```bash
npx @0xdiesis/evm-benchmark --rpc-endpoints http://localhost:8545 --txs 2000 --fund
```

### Option B: Download a prebuilt binary

Download from [GitHub Releases](https://github.com/0xDiesis/evm-benchmark/releases):

```bash
# Download and extract (example for macOS ARM)
tar xzf evm-benchmark-v0.1.0-aarch64-apple-darwin.tar.gz

# Download bench-targets (chain configs, Docker compose files, scripts)
./evm-benchmark --setup
```

### Option C: Clone and build from source

```bash
git clone https://github.com/0xDiesis/evm-benchmark.git
cd evm-benchmark
make build
```

## Quick Start

```bash
# Using the standalone binary (auto-prompts to download bench-targets if missing)
evm-benchmark --rpc-endpoints http://localhost:8545 --txs 2000 --fund

# Using the Makefile (requires cloned repo)
make bench CHAIN=sonic MODE=burst TXS=2000

# Compare two chains with identical settings
make compare CHAINS="diesis sonic" MODE=burst

# Run all modes (burst + sustained + ceiling)
make compare-all CHAINS="diesis sonic" ENV=clean
```

## Makefile Targets

All targets accept override variables on the command line. Defaults are shown in parentheses.

### Build and Quality

| Target | Description |
|--------|-------------|
| `make build` | Build the benchmark harness (release) |
| `make build-debug` | Build the benchmark harness (debug) |
| `make test` | Run harness unit tests |
| `make clippy` | Run clippy with `-D warnings` |
| `make fmt` | Format all Rust code |
| `make check` | Run fmt check + clippy + tests |

### Single-Chain Benchmarks

| Target | Description |
|--------|-------------|
| `make bench` | Run a single benchmark |
| `make bench-all` | Run burst + sustained + ceiling sequentially |

**Variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `CHAIN` | `diesis` | Target chain (see `make chains` for options) |
| `MODE` | `burst` | Execution mode: `burst`, `sustained`, `ceiling` |
| `ENV` | `clean` | Environment: `clean`, `geo-global`, `geo-us`, `geo-eu`, `geo-degraded`, `geo-intercontinental` |
| `TXS` | `10000` | Transaction count (burst mode) |
| `TPS` | `200` | Target TPS (sustained/ceiling modes) |
| `DURATION` | `30` | Duration in seconds (sustained mode) |
| `SENDERS` | `200` | Number of sender accounts |
| `BATCH_SIZE` | `200` | RPC batch submission size |
| `WORKERS` | `8` | Number of async worker tasks |
| `TAG` | *(auto)* | Custom tag for the run directory |
| `TEST_MODE` | `transfer` | Test mode: `transfer` or `evm` |

**Examples:**

```bash
# Burst 5000 txs against Sonic
make bench CHAIN=sonic TXS=5000

# Sustained 500 TPS for 60 seconds
make bench MODE=sustained TPS=500 DURATION=60

# Ceiling test with EVM mixed workload
make bench MODE=ceiling TEST_MODE=evm

# Burst under simulated global latency
make bench ENV=geo-global

# Full suite (burst + sustained + ceiling) with clean restarts
make bench-all CHAIN=sonic ENV=clean
```

### Head-to-Head Comparisons

| Target | Description |
|--------|-------------|
| `make compare` | Compare 2+ chains with identical settings |
| `make compare-all` | Compare across all modes (burst + sustained + ceiling) |

Chains are benchmarked sequentially with isolation — only one chain runs at a time to prevent resource contention.

**Variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `CHAINS` | `"diesis sonic"` | Space-separated chain names to compare |
| `MODE` | `burst` | Mode (or `all` for compare-all) |
| `ENV` | `clean` | Environment for all chains |

**Examples:**

```bash
# Diesis vs Sonic burst comparison
make compare CHAINS="diesis sonic"

# Three-way comparison across all modes
make compare-all CHAINS="diesis sonic avalanche"

# Compare under geographic latency
make compare CHAINS="diesis sonic" ENV=geo-global
```

### Results Management

| Target | Description |
|--------|-------------|
| `make results` | List all benchmark results |
| `make results-latest` | Show the most recent result |
| `make results-compare RUNS="..."` | Ad-hoc compare 2+ runs |
| `make results-summary` | Aggregate stats across all runs |

**Filter variables** (for `results` and `results-latest`):

| Variable | Default | Description |
|----------|---------|-------------|
| `FILTER_CHAIN` | *(empty)* | Filter results by chain name |
| `FILTER_MODE` | *(empty)* | Filter results by mode |

**Examples:**

```bash
# List all results
make results

# List only Diesis ceiling results
make results FILTER_CHAIN=diesis FILTER_MODE=ceiling

# Show the latest Sonic result
make results-latest FILTER_CHAIN=sonic

# Compare two specific runs side by side
make results-compare RUNS="bench-targets/results/runs/diesis/burst/20260402-211524_clean bench-targets/results/runs/sonic/burst/20260402-212030_clean"

# Aggregate stats grouped by chain
make results-summary
```

### Info

| Target | Description |
|--------|-------------|
| `make chains` | List all registered chains |
| `make help` | Show all targets with descriptions |

## Execution Modes

### Burst

Submit all transactions as fast as possible and measure peak throughput. Transactions are pre-signed, then submitted in configurable waves with batch RPC calls.

```bash
make bench MODE=burst TXS=5000
```

### Sustained

Maintain a target TPS for a fixed duration to measure stability and latency under steady load. Workers use rate-limited intervals to achieve the target rate.

```bash
make bench MODE=sustained TPS=500 DURATION=60
```

### Ceiling

Ramp TPS incrementally until the chain saturates. Starts at `TPS` (or 100, whichever is higher), increases by steps, and detects saturation via pending ratio, error rate, and TPS ratio thresholds.

By default, orchestrated runs restart the chain between ceiling ramp steps and perform a short warmup block-progress check. This avoids residual pending txs from one load level contaminating the next.

```bash
make bench MODE=ceiling TPS=200
```

Override controls (optional):

- `BENCH_CEILING_RESTART_BETWEEN_STEPS=true|false`
- `BENCH_CEILING_RESTART_CMD="<command>"`
- `BENCH_CEILING_COOLDOWN_SECS=<n>`
- `BENCH_CEILING_WARMUP_SECS=<n>`
- `BENCH_CEILING_RESTART_READY_TIMEOUT_SECS=<n>`

## Environment Modes

### Clean (`ENV=clean`)

Stops the chain, removes volumes, and restarts from genesis. Ensures no prior state affects results. This is the default.

### Geographic Latency (`ENV=geo-*`)

Applies network topology simulation before benchmarking using Linux TC (traffic control) with pairwise latency between validator nodes.

| Mode | Profile | RTT Range | Scenario |
|------|---------|-----------|----------|
| `geo-global` | `global-spread` | 60–240ms | US-East / US-West / EU-Frankfurt / Asia-Tokyo |
| `geo-us` | `us-distributed` | 20–60ms | Four US regions |
| `geo-eu` | `eu-cluster` | 2–90ms | 3 EU co-located + 1 US outlier |
| `geo-degraded` | `degraded-wan` | 80–200ms | Congested network, high jitter |
| `geo-intercontinental` | `intercontinental` | 75–340ms | US / EU / Asia / South America |

Topology is automatically applied before and cleared after each run.

```bash
# Single chain under global latency
make bench ENV=geo-global

# Compare chains under latency
make compare CHAINS="diesis sonic" ENV=geo-global
```

## Registered Chains

View with `make chains`. Currently registered:

| Chain | Type | Validators | Port Range |
|-------|------|------------|------------|
| `diesis` | L1 — Mysticeti BFT | 4 | 8545–8575 |
| `sonic` | L1 — Lachesis aBFT | 4 | 18545–18845 |
| `sei` | L1 — sei-tendermint BFT | 4 | 28545–28551 |
| `avalanche` | L1 — Snowman consensus | 5 | 9650 |
| `berachain` | L1 — CometBFT + beacon-kit | 4 | dynamic |
| `reth` | Reference EVM (reth --dev) | 1 | 38545 |
| `geth` | Reference EVM (geth --dev) | 1 | 18889 |
| `anvil` | Dev tool (no consensus) | 1 | 18888 |
| `bsc` | L1 — Parlia PoSA | 3 | 8545 |
| `cosmos` | L1 — CometBFT (Evmos) | 1 | 8545 |
| `arbitrum` | L2 — Nitro optimistic rollup | 1 sequencer | 8547 |
| `optimism` | L2 — OP Stack (Supersim) | 1 | 9545 |
| `scroll` | L2 — zkEVM (l2geth standalone) | 1 | 48545 |
Each chain has its own setup under `bench-targets/chains/<name>/` with a Makefile for standalone use (`make up`, `make down`, `make bench`).

## Report Organization

All results are stored under `bench-targets/results/` (gitignored) with a hierarchical layout:

```
bench-targets/results/
├── runs/                              # Individual benchmark runs
│   └── <chain>/<mode>/<timestamp>_<tag>/
│       ├── report.json                # Harness output (TPS, latency, etc.)
│       ├── meta.json                  # Run metadata (git sha, chain config, system info)
│       └── console.log                # Captured stdout/stderr
│
├── comparisons/                       # Head-to-head comparisons
│   └── <timestamp>_<chains>_<mode>/
│       ├── summary.json               # Side-by-side metrics
│       ├── summary.md                 # Markdown comparison table
│       └── <chain>.json               # Per-chain reports
│
├── sweeps/                            # Parameter sweep results
│   └── <timestamp>_<param>/
│       ├── summary.json               # All values with metrics
│       ├── summary.md                 # Markdown results table
│       ├── <value>/report.json        # Per-value run
│       └── best -> <value>/           # Symlink to best performer
│
└── index.json                         # Machine-readable catalog of all runs
```

Each run directory also maintains `latest` symlinks at every level for quick access.

### Metadata

Every run captures `meta.json` with:

- Git SHA and branch of the benchmark repo
- Chain configuration (block period, ordering window, execution mode, commitment type, etc.)
- Benchmark parameters (txs, senders, batch size, workers, TPS, duration)
- System information (OS, architecture, CPU count, memory)

## Standalone Usage

The harness works against any EVM-compatible RPC endpoint without Docker or the bench-targets:

```bash
# Benchmark any chain directly
evm-benchmark \
    --rpc-endpoints http://your-node:8545 \
    --chain-id 1 \
    --execution burst \
    --txs 1000 \
    --senders 100 \
    --fund \
    --out my-report.json
```

Or via `cargo run` from the repo:

```bash
cargo run -p evm-benchmark --release -- \
    --rpc-endpoints http://your-node:8545 \
    --chain-id 1 --txs 2000 --execution burst --fund
```

### Harness CLI Reference

| Flag | Default | Description |
|------|---------|-------------|
| `--rpc-endpoints` | `http://localhost:8545` | Comma-separated RPC URLs (round-robin failover) |
| `--ws` | `ws://localhost:8546` | WebSocket endpoint for block tracking |
| `--chain-id` | `19803` (auto-detects from RPC if available) | EVM chain ID |
| `--execution` | `burst` | Mode: `burst`, `sustained`, `ceiling` |
| `--test` | `transfer` | Test type: `transfer` or `evm` |
| `--txs` | `10000` | Transaction count (burst) |
| `--tps` | `100` | Target TPS (sustained/ceiling) |
| `--duration` | `60` | Duration in seconds (sustained) |
| `--senders` | `200` | Sender account count |
| `--waves` | `8` | Submission waves (burst) |
| `--wave-delay-ms` | `0` | Delay between waves in ms (burst) |
| `--workers` | `8` | Async worker count |
| `--batch-size` | `100` | RPC batch size |
| `--fund` | off | Auto-fund sender accounts via MultiSend |
| `--out` | `report.json` | JSON report output path |
| `--bench-name` | `evm_bench_v1` | Label for results |
| `--quiet` | off | Suppress console output |
| `--submission-method` | `http` | `http` or `websocket` |
| `--retry-profile` | `moderate` | Retry profile: `off`, `light`, `moderate`, `aggressive` |
| `--finality-confirmations` | `0` | Blocks deep before tx counts as confirmed |
| `--setup` | off | Download bench-targets from GitHub and exit |
| `--update-targets` | off | Re-download bench-targets before running |
| `--targets-branch` | `main` | GitHub branch to download bench-targets from |

Environment variables: `BENCH_KEY` (comma-separated private keys), `BENCH_TX_CACHE_DIR` (pre-signed tx cache), `BENCH_EVM_TOKENS` / `BENCH_EVM_PAIRS` / `BENCH_EVM_NFTS` (contract addresses for EVM mode).

## Repository Layout

```
.
├── Makefile                       # All benchmark targets
├── bench-targets/
│   ├── scripts/                   # Orchestration scripts
│   │   ├── lib.sh                 # Chain registry, shared functions
│   │   ├── bench.sh               # Single run orchestrator
│   │   ├── compare.sh             # Multi-chain comparison
│   │   ├── sweep.sh               # Parameter sweep orchestrator
│   │   ├── sweep-profiles.sh      # Sweep profile definitions
│   │   ├── results.sh             # Results listing and lookup
│   │   └── meta.sh                # Metadata collection
│   ├── chains/                    # Per-chain configs (Makefile, docker-compose, run-bench.sh)
│   │   ├── diesis/
│   │   ├── sonic/
│   │   ├── sei/
│   │   ├── avalanche/
│   │   ├── berachain/
│   │   ├── bsc/
│   │   └── cosmos/
│   ├── network-topology/          # Geo-latency simulation (TC pairwise + Pumba chaos)
│   ├── run-comparison.sh          # Legacy comparison script
│   ├── test-suite.sh              # Legacy test suite
│   └── results/                   # Generated output (gitignored)
├── crates/
│   └── evm-benchmark/             # Rust CLI benchmark harness
│       ├── src/
│       │   ├── modes/             # burst.rs, sustained.rs, ceiling.rs
│       │   ├── submission/        # RPC dispatch, block tracking
│       │   ├── signing/           # Parallel batch signing
│       │   ├── generators/        # Transaction generators (transfer, EVM mix)
│       │   ├── analytics/         # Bottleneck detection, recommendations
│       │   ├── reporting/         # JSON, HTML, ASCII, Markdown reports
│       │   └── setup.rs           # Bench-target download from GitHub
│       ├── tests/                 # E2E and integration tests
│       └── benches/               # Criterion micro-benchmarks
├── npm/
│   └── evm-benchmark/             # npm package (@0xdiesis/evm-benchmark)
└── docs/
    ├── evm-benchmark.md           # Architecture notes
    └── evm-benchmark-features.md  # Detailed feature reference
```

## Further Reading

- [docs/evm-benchmark.md](docs/evm-benchmark.md) — architecture and design notes
- [docs/evm-benchmark-features.md](docs/evm-benchmark-features.md) — detailed harness capabilities
- [bench-targets/network-topology/README.md](bench-targets/network-topology/README.md) — geo-latency simulation guide
