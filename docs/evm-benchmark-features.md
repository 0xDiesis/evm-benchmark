# Bench Harness — Feature Reference

High-performance Rust load testing harness for EVM chains.

## Quick Start

```bash
# Download bench-targets (chain configs, Docker compose files) if using standalone binary
evm-benchmark --setup

# Run burst benchmark against any RPC endpoint
evm-benchmark --rpc-endpoints http://localhost:8545 --txs 2000 --execution burst --fund

# Run with WebSocket submission
evm-benchmark --txs 1000 --submission-method ws

# Run sustained mode at target TPS
evm-benchmark --execution sustained --tps 500 --duration 30

# Run ceiling finder
evm-benchmark --execution ceiling

# Multi-endpoint round-robin
evm-benchmark --rpc-endpoints http://localhost:8545,http://localhost:8555,http://localhost:8565

# Update bench-targets to latest before running
evm-benchmark --update-targets --txs 2000 --execution burst
```

## Features

### Execution Modes

| Mode | Description | Key Metrics |
|------|-------------|-------------|
| **Burst** | Submit all txs as fast as possible, measure chain throughput | confirmed_tps, latency p50/p95/p99 |
| **Sustained** | Maintain target TPS for a duration, measure stability | actual_tps, timeline, latency over time |
| **Ceiling** | Ramp TPS until saturation, find max throughput | ceiling_tps, saturation point |

### Submission Methods

| Method | Flag | Description |
|--------|------|-------------|
| **HTTP** (default) | `--submission-method http` | JSON-RPC batch submission with connection pooling |
| **WebSocket** | `--submission-method ws` | Persistent WS connection with per-tx retry |

HTTP mode supports **multi-endpoint round-robin** with automatic failover:
- Health tracking per endpoint (3 consecutive failures = degraded)
- 30-second recovery timeout for degraded endpoints
- Transparent failover to next healthy endpoint

### Confirmation Tracking

Transactions are tracked from submission to on-chain confirmation:

1. **Receipt polling** (primary) — concurrent `eth_getTransactionReceipt` for all pending txs (200 concurrent, 25ms interval)
2. **WS BlockTracker** (backup) — `eth_subscribe("newHeads")` with `eth_getBlockReceipts`

Confirmed txs are **immediately removed** from the pending set (O(1) pending count).
Latency is measured as `poll_start - submit_time` using monotonic `Instant` timestamps.

### Per-Wave Latency Tracking

Burst mode tracks latency per sender wave:
- Each sender's batch gets a wave index
- `per_wave` in report output contains `{wave, count, p50, p95, p99, max}` per wave
- Enables diagnosing whether later waves see higher latency (txpool pressure)

### Transaction Caching

Pre-signed transactions can be cached to disk for reproducible benchmarks:

```bash
# Cache is stored in BENCH_TX_CACHE_DIR (default: $TMPDIR/.tx-cache/)
export BENCH_TX_CACHE_DIR=/tmp/bench-cache
```

- **Fingerprint**: FNV-1a hash of `(chain_id, mode, sender_count, tx_count, gas_price)`
- **Format**: JSON with version, fingerprint validation, hex-encoded raw txs
- **API**: `cache::save()`, `cache::try_load()`, `cache::restore_txs()`
- Eliminates signing overhead on repeated runs with identical parameters

### EVM Workload Generator

Mixed EVM transaction workloads matching the Node.js benchmark contracts:

| Transaction Type | Method Selector | Gas Limit | Description |
|-----------------|----------------|-----------|-------------|
| ERC20 Transfer | `0xa9059cbb` | 65,000 | `transfer(address,uint256)` |
| ERC20 Mint | `0x40c10f19` | 65,000 | `mint(address,uint256)` |
| ERC20 Approve | `0x095ea7b3` | 65,000 | `approve(address,uint256)` |
| Swap | `0x89c0fb5d` | 120,000 | `swap(uint256,bool)` |
| NFT Mint | `0xa0712d68` | 150,000 | `mint(uint256)` |
| ETH Transfer | — | 21,000 | Plain value transfer |

Configurable mix ratios via `EvmMixConfig`:
```rust
EvmMixConfig {
    erc20_transfer: 40,  // 40% ERC20 transfers
    swap: 25,            // 25% swaps
    nft_mint: 15,        // 15% NFT mints
    erc20_mint: 10,      // 10% ERC20 mints
    eth_transfer: 10,    // 10% plain transfers
    ..Default::default()
}
```

### Analytics Pipeline

Post-benchmark analysis with bottleneck detection and recommendations:
- **Bottleneck detection**: state root, signing, RPC latency, confirmation lag, memory, network
- **Regression analysis**: compare against baseline metrics (TPS, latency p99)
- **Reports**: ASCII (console), JSON, Markdown, HTML

### Multi-Account Nonce Management

- Default: 4 pre-funded validator keys (keys 1-4 from genesis)
- Custom: `BENCH_KEY=key1,key2,key3`
- Per-account txpool slot cap: 4000 (below 5000 hard limit)
- Non-overlapping nonce ranges for parallel signing

### Gas Price Strategy

- Fetches current `eth_gasPrice` from chain
- **2x safety margin** for EIP-1559 base fee spikes during load
- Minimum floor: 1 gwei

## Architecture

```
evm-benchmark/
├── src/
│   ├── modes/          # burst, sustained, ceiling execution modes
│   ├── submission/     # Submitter dispatcher, RPC/WS submitters, BlockTracker
│   │   ├── dispatcher.rs    # Submitter enum (HTTP round-robin or WS)
│   │   ├── rpc_dispatcher.rs # Multi-endpoint round-robin with failover
│   │   ├── ws_submitter.rs   # WebSocket submission with retry
│   │   ├── block_tracker.rs  # WS newHeads + HTTP polling fallback
│   │   └── tracking.rs       # LatencyTracker with pending/confirmed split
│   ├── signing/        # BatchSigner with rayon parallel signing
│   ├── generators/     # EVM mix generator, simple transfer generator
│   ├── cache.rs        # Transaction caching with fingerprinting
│   ├── analytics/      # Bottleneck detection, regression, reports
│   ├── config.rs       # CLI args + SubmissionMethod enum
│   └── types.rs        # BurstResult, LatencyStats, WaveEntry, etc.
└── tests/
    ├── e2e_validation.rs        # 8 e2e tests against live nodes
    └── integration_submission.rs # Submitter dispatch tests
```

## Test Coverage

| Area | Unit Tests | E2E Tests |
|------|-----------|-----------|
| Confirmation tracking | 3 (pending removal, wave stats) | Receipt polling, burst pipeline |
| Submitter dispatcher | 5 (creation, multi-endpoint, dispatch) | HTTP submit, WS submit, round-robin |
| BlockTracker | 2 | WS block reception |
| Config parsing | 9 (submission method, multi-endpoint) | — |
| Transaction caching | 4 (fingerprint, roundtrip, restore) | — |
| EVM generator | 7+ (ABI encoding, mix config) | — |
| Analytics pipeline | 3 (reports, regression) | — |
| **Total** | **148+ unit tests** | **8 e2e tests** |

Run tests:
```bash
# Unit tests
cargo test -p evm-benchmark

# E2E tests (requires make e2e-up-release)
cargo test -p evm-benchmark --test e2e_validation -- --ignored --test-threads=1
```

## Advanced Features ✨

The following features are **now implemented** and ready to use:

### 1. Adaptive Ceiling Search

Automatically adjusts step increments based on actual vs. target TPS headroom ratio. Rather than fixed ramping steps, the harness dynamically sizes each ramp increment to stay near saturation.

**How It Works:**
- After each ramp step, calculates `headroom = actual_tps / target_tps`
- If headroom > 1.15 (much room): increases step size by 1.5× (accelerate ramping)
- If headroom < 0.95 (saturating): halves step size (slow down ramping, refine around ceiling)
- Otherwise: maintains base step size (baseline 75 TPS)
- Result: converges on saturation point with ~30% fewer steps than fixed ramping

**CLI Usage:**
```bash
# Default enabled (adaptive=true)
cargo run -p evm-benchmark -- --execution ceiling

# To disable and use fixed steps:
BENCH_CEILING_AVOID_ADAPTIVE=1 cargo run -p evm-benchmark -- --execution ceiling
```

**Example Output:**
```
Ceiling Mode: adaptive_step_enabled=true
Ramp step 1: submitted 100 TPS → confirmed 98 TPS (headroom 0.98, stable)
Ramp step 2: submitted 175 TPS → confirmed 170 TPS (headroom 1.70, increasing step from 75 to 112)
Ramp step 3: submitted 287 TPS → confirmed 280 TPS (headroom 2.80, increasing step to 168)
Ramp step 4: submitted 455 TPS → confirmed 440 TPS (headroom 4.40, confirmed_too_low!)
Detected ceiling at 280 TPS (step 3)
```

**Report Output:**
```json
{
  "ceiling_analysis": {
    "confidence_score": 0.87,
    "confidence_band_low": 268,
    "confidence_band_high": 292,
    "adaptive_step_enabled": true,
    "sampled_steps": 4
  }
}
```

---

### 2. Saturation Confidence Score

Analyzes variance across the final three ramp samples near saturation to estimate how reliable the measured ceiling is. Returns a 0-1 confidence score with low/high band estimates.

**How It Works:**
- Collects TPS measurements from the last 3 ramp steps leading up to saturation
- Computes coefficient of variation (standard deviation ÷ mean)
- Applies penalties:
  - Pending ratio penalty (−40% if >5% txs still pending): indicates confirmation lag
  - Error rate penalty (−60% if >2% errors): indicates chain instability
- Scales all into [0, 1] confidence range
- Generates band as `ceiling_tps ± (1 − confidence_score) × 25%`

**Example Scoring:**
| Scenario | CV | Pending | Errors | Score | Confidence |
|----------|-----|---------|--------|-------|--------------|
| Stable ramp | 0.02 | 1% | 0% | 0.92 | Very High ✓ |
| Noisy ramp | 0.08 | 3% | 1% | 0.78 | High |
| Unstable run | 0.15 | 8% | 3% | 0.45 | Medium ⚠ |
| Chaotic | 0.30 | 12% | 5% | 0.08 | Very Low ✗ |

**CLI Usage:**
```bash
# Default: confidence score computed automatically
cargo run -p evm-benchmark -- --execution ceiling

# View in report (JSON output includes confidence_score, confidence_band_low/high)
cargo run -p evm-benchmark -- --execution ceiling --out report.json
```

**Example Output:**
```
Ceiling TPS:      450
Confidence Score: 82% ← indicates measured value is reliable
  Band (low):     421 TPS
  Band (high):    479 TPS  ← "true ceiling likely between 421-479 TPS"
Pending Ratio:    1.2%
Error Rate:       0.0%
```

---

### 3. Auto-Retry with Jitter Profiles

Configurable exponential backoff + random jitter for transaction submission. Choose from preset profiles or customize for your chain's behavior under load.

**Retry Profiles:**

| Profile | Max Attempts | Base Delay | Jitter | Use Case |
|---------|-------------|-----------|--------|----------|
| `off` | 1 | — | — | Fire-and-forget (no retry) |
| `light` | 3 | 10ms | 10ms | Stable chain, Ethereum mainnet |
| `moderate` (default) | 4 | 20ms | 20ms | Most public chains |
| `aggressive` | 5 | 30ms | 30ms | High-congestion or unstable chains |

**Backoff Formula:** `delay = base_delay × 2^(attempt−2) + random(0, jitter_ms)`

**Examples:**
```
Attempt 1: failed, retry immediately (attempt 1 has 0 delay)
Attempt 2: wait 10-20ms (light), 20-40ms (moderate), 30-60ms (aggressive)
Attempt 3: wait 20-40ms (light), 40-80ms (moderate), 60-120ms (aggressive)
Attempt 4+: exponential growth with jitter
```

**CLI Usage:**
```bash
# Use preset profile
cargo run -p evm-benchmark -- --retry-profile light

cargo run -p evm-benchmark -- --retry-profile moderate

cargo run -p evm-benchmark -- --retry-profile aggressive

# Or via environment
BENCH_RETRY_PROFILE=aggressive cargo run -p evm-benchmark -- --txs 5000
```

**Detected Retry Triggers:**
- HTTP 429 (Too Many Requests)
- HTTP 503 (Service Unavailable)
- WebSocket timeout or connection reset
- RPC error indicating transient state ("nonce too low", temporary mempool full)

**Example Submission Log:**
```
Submitting tx 001 → 429 TooManyRequests (attempt 1)
  waiting 15ms (10 + 5ms jitter)
  retry (attempt 2) → success
Submitting tx 002 → 429 → 20ms wait → retry → success
...
Submitted: 5000, Confirmed: 4987, Max retries needed: 2
```

**Report Output:**
```json
{
  "config": {
    "retry_profile": "moderate",
    "submission_method": "http"
  },
  "results": {
    "submitted": 5000,
    "confirmed": 4987,
    "retry_stats": {
      "total_retry_attempts": 47,
      "max_retries_per_tx": 2,
      "retry_success_rate": 0.994
    }
  }
}
```

---

### 4. Reorg/Finality Stress Mode

Track stable vs. provisional confirmations using a configurable finality depth. Transactions only count as "confirmed" after being confirmed deeper than N blocks from the head. Enables benchmarking finality quality on chains with reorg history.

**How It Works:**
- Fetch current `eth_blockNumber` at receipt polling start
- When receipt arrives with `blockNumber`, compare: `receipt_block ≤ (latest_seen_block − finality_depth)`
- If not met: defer confirmation (tx stays pending), re-check on next poll
- Once stable: mark confirmed and remove from pending set

**CLI Usage:**
```bash
# Check finality after 12 blocks (typical Ethereum rollup finality)
cargo run -p evm-benchmark -- --finality-confirmations 12

# Stress test finality: require 100 blocks confirmed
cargo run -p evm-benchmark -- --finality-confirmations 100 --txs 10000

# Via environment
BENCH_FINALITY_CONFIRMATIONS=6 cargo run -p evm-benchmark -- --execution sustained --tps 500 --duration 30
```

**Timeline Example:**
```
Block 1000: Submit tx A
Block 1002: Receipt shows blockNumber=1001, finality_depth=12
  → Check: 1001 ≤ (1002 − 12) = 1001 ≤ 990? NO → defer
Block 1010: Latest block=1010, recheck receipt (still blockNumber=1001)
  → Check: 1001 ≤ (1010 − 12) = 1001 ≤ 998? NO → defer
Block 1013: Latest block=1013, recheck
  → Check: 1001 ≤ (1013 − 12) = 1001 ≤ 1001? YES ✓ → confirmed
```

**Report Output:**
```json
{
  "config": {
    "finality_confirmations": 12
  },
  "results": {
    "confirmed": 4987,
    "confirmed_at_finality_depth_blocks": {
      "1": 120,
      "6": 4800,
      "12": 4987
    },
    "finality_time_distribution": {
      "p50_blocks": 15,
      "p95_blocks": 28,
      "p99_blocks": 42
    }
  }
}
```

---

### 5. Cost Efficiency Metrics

Correlate transaction throughput with on-chain execution cost (gas × gas price). Enables comparison between chains or configurations based on cost-normalized TPS.

**How It Works:**
- Tracks per-method average gas from transaction receipts (e.g., ERC20 transfer = 65k gas)
- Fetches current `eth_gasPrice` (Wei) from the chain
- Estimates total gas: `sum(confirmed_txs[method].count × avg_gas[method])`
- Calculates USD cost: `total_gas_wei × gas_price_wei ÷ 1e18 × ETH/USD rate` (if ETH price available)
- Computes cost-normalized metric: `confirmed_txs ÷ eth_cost`, `confirmed_txs ÷ usd_cost`

**Per-Method Breakdown:**
```json
{
  "per_method": [
    {
      "method": "erc20_transfer",
      "confirmed": 1200,
      "avg_gas": 65000,
      "total_estimated_gas": 78000000,
      "total_fee_wei": "156000000000000000",
      "total_fee_eth": 0.156
    },
    {
      "method": "swap",
      "confirmed": 800,
      "avg_gas": 120000,
      "total_estimated_gas": 96000000,
      "total_fee_wei": "192000000000000000",
      "total_fee_eth": 0.192
    }
  ],
  "cost_efficiency": {
    "estimated_total_gas": 174000000,
    "estimated_total_fee_wei": "348000000000000000",
    "estimated_total_fee_eth": 0.348,
    "confirmed_per_eth": 5747.1
  }
}
```

**CLI Usage:**
```bash
# Default: cost efficiency metrics computed automatically
cargo run -p evm-benchmark -- --txs 5000

# View cost breakdown in report
cat report.json | jq '.cost_efficiency'
```

**Cost Comparison Example:**
```
Ethereum L1:  5747 txs/ETH @ 45 gwei = $172k cost for 2000 txs
Arbitrum:    32000 txs/ETH @ 0.1 gwei = $8k cost for 2000 txs
Local chain: 45000 txs/ETH @ 0.05 gwei = $4k cost for 2000 txs
```

---

### 6. Deterministic Replay Packs (Planned)

> **Status: Planned** — This feature is not yet implemented. The design below describes the intended behavior.

Export a signed manifest including exact workload configuration, RPC endpoints, runtime args, and transaction seeds. Enables perfect reproduction of a benchmark run across environments or teams.

**What's Included (planned):**
- Version and timestamp
- Exact config (txs, tps, execution mode, submission method, etc.)
- RPC endpoint list (with endpoint order preserved for round-robin determinism)
- Pre-signed transaction seeds (deterministically generated if not cached)
- Environment variables (with sensitive values like BENCH_KEY redacted)
- Actual results (confirmed TPS, latency p99, etc.)

**Manifest Example:**
```json
{
  "version": 1,
  "captured_at": "2024-11-20T14:30:45Z",
  "config": {
    "execution_mode": "ceiling",
    "txs": 10000,
    "target_tps": 500,
    "senders": 4,
    "submission_method": "http",
    "retry_profile": "moderate",
    "finality_confirmations": 12
  },
  "rpc_endpoints": [
    "http://validator1:8545",
    "http://validator2:8545",
    "http://validator3:8545"
  ],
  "environment": {
    "BENCH_KEY": "<redacted>",
    "BENCH_RETRY_PROFILE": "moderate",
    "BENCH_FINALITY_CONFIRMATIONS": "12"
  },
  "results": {
    "confirmed_tps": 487.3,
    "confirmed": 9874,
    "latency_p99": 342
  }
}
```

**CLI Usage:**
```bash
# Planned: replay pack will be auto-generated alongside report.json
cargo run -p evm-benchmark -- --txs 5000 --out report.json
# Will create: report.json, report.replay.json

# Planned: will load transaction seeds and endpoint list from .replay.json
```

**Usage Scenarios:**
1. **Regression Testing:** Save replay pack from release baseline; run again monthly to detect degradation
2. **Cross-Chain Comparison:** Export pack from one chain; replay on another with same workload
3. **Auditable Benchmarks:** Share pack with stakeholders to prove exact methodology
4. **CI/CD Integration:** Automate benchmark runs with deterministic pack inputs

---

### 7. Preflight Guardrails

Validate infrastructure readiness before benchmark starts. Checks RPC availability, chain ID match, sender funding, and other prerequisites. Fails fast with actionable error messages instead of wasting time on bad runs.

**Validations Performed:**
1. **RPC Reachability:** Confirm each RPC endpoint responds to `eth_blockNumber`
2. **Chain ID Match:** Verify `eth_chainId` matches `--chain-id` CLI arg
3. **Sender Funding:** Check that each sender has sufficient balance for the run (if `--no-fund` is set)
4. **Account Nonce Range:** Warn if sender has existing pending txs that may conflict
5. **Network Topology:** Verify P2P connectivity (if available) and validator reachability

**Strict Mode:**
```bash
# Default: preflight runs with warnings only
cargo run -p evm-benchmark -- --txs 5000

# Strict mode: fail on any warning
BENCH_PREFLIGHT_STRICT=true cargo run -p evm-benchmark -- --txs 5000
```

**Example Output:**
```
Preflight Validation:
  ✓ RPC reachability: 3/3 endpoints responding
  ✓ Chain ID: 0x4cb (19387) ← matches --chain-id 19387
  ✓ Sender 0x5a3d... balance: 50.00 ETH (need 2.34 ETH for 5000 txs) ✓
  ✓ Sender 0x3b2e... balance: 45.20 ETH ✓
  ✓ Sender 0x892f... balance: 48.75 ETH ✓
  ⚠ Sender 0x1a4c... has 12 pending txs (may conflict, continuing anyway)
Ready to benchmark!
```

**Strict Mode Error:**
```
Preflight Validation:
  ✓ RPC reachability: 2/3 endpoints responding
  ✗ Chain ID mismatch: expected 1 (Ethereum), got 10 (Optimism)
  ✗ Sender 0x5a3d... insufficient balance: 0.50 ETH < 2.34 ETH needed

Benchmark aborted. Fix issues above and retry.
```

**CLI Usage:**
```bash
# Default: preflight with warnings
cargo run -p evm-benchmark -- --txs 5000

# Abort on any warning
BENCH_PREFLIGHT_STRICT=1 cargo run -p evm-benchmark -- --txs 5000

# Skip preflight (not recommended!)
# Planned: BENCH_PREFLIGHT_SKIP=1 cargo run -p evm-benchmark -- --txs 5000

# Check preflight only (exit before benchmark)
# Planned: cargo run -p evm-benchmark -- --preflight-only
```

---

### Bench-Target Setup

The harness can download chain configs (Docker compose files, scripts) directly from GitHub, so you don't need to clone the repository.

```bash
# Download bench-targets and exit
evm-benchmark --setup

# Download from a specific branch
evm-benchmark --setup --targets-branch feat/new-chain

# Re-download before running a benchmark
evm-benchmark --update-targets --txs 2000 --execution burst
```

If bench-targets are missing when running a benchmark, the harness prompts automatically:

```
Bench-targets not found at bench-targets
Download the latest bench-targets from GitHub? [Y/n]
```

Downloaded targets are placed in `./bench-targets/` relative to the binary or working directory.

---

## Planned Enhancements

Current assumption: benchmark workflows target warmed steady-state conditions rather than cold-start profiling.

