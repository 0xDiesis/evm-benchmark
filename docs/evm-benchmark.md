# Rust Load Testing Harness

## Overview

High-performance Rust load testing harness for EVM network benchmarking,
with chain-agnostic presets and debugging workflows. It replaces the older
Node.js benchmarking system and achieves 60-120x faster signing and
10-17x faster submission with production-ready code quality, comprehensive test
coverage, and validated E2E performance.

For detailed feature documentation and usage, see
[evm-benchmark-features.md](evm-benchmark-features.md).

### Key Results

| Metric           | Node.js       | Rust           | Speedup     |
|------------------|---------------|----------------|-------------|
| Signing Rate     | 1-3k txs/sec  | 33k-150k/sec   | 60-120x     |
| Max TPS          | 100-150       | 3,740+         | 25-37x      |
| Memory           | 200-500 MB    | <100 MB        | 2-5x lower  |
| GC Pauses        | Yes           | None           | -           |

---

## Architecture

```
crates/evm-benchmark/
├── Cargo.toml
├── src/
│   ├── main.rs                          # CLI entry point
│   ├── config.rs                        # Configuration parser + SubmissionMethod enum
│   ├── types.rs                         # Shared types (BurstResult, WaveEntry, etc.)
│   ├── cache.rs                         # Transaction caching with fingerprinting
│   ├── signing/
│   │   └── batch.rs                     # Rayon parallel signing (BatchSigner)
│   ├── submission/
│   │   ├── dispatcher.rs                # Submitter enum (HTTP round-robin or WS)
│   │   ├── rpc.rs                       # JSON-RPC batch submission
│   │   ├── rpc_dispatcher.rs            # Multi-endpoint round-robin with failover
│   │   ├── ws_submitter.rs              # WebSocket submission with retry
│   │   ├── tracking.rs                  # LatencyTracker (pending/confirmed split)
│   │   └── block_tracker.rs             # WS newHeads + HTTP polling fallback
│   ├── generators/
│   │   ├── simple_transfer.rs           # Simple ETH transfers (21k gas)
│   │   ├── evm_mix.rs                   # EVM mixed workloads (7 method types)
│   │   └── contract_deploy.rs           # Contract deployment helpers
│   ├── modes/
│   │   ├── burst.rs                     # Burst mode + receipt polling
│   │   ├── sustained.rs                 # Sustained mode (rate-limited)
│   │   └── ceiling.rs                   # Ceiling mode (saturation detection)
│   ├── analytics/                       # Bottleneck detection, reports
│   └── validators/                      # Endpoint health monitoring
├── tests/
│   ├── e2e_validation.rs                # 8 E2E tests against live nodes
│   └── integration_submission.rs        # Submitter dispatch tests
└── benches/
    ├── signing.rs                       # Signing benchmarks
    └── submission.rs                    # Submission benchmarks
```

### Key Design Decisions

- **Confirmation tracking**: Confirmed txs are immediately removed from the
  pending `DashMap` (matching Node.js `pending.delete()` pattern). `pending_count()`
  is O(1) via `DashMap::len()`. Confirmed latencies stored in a separate `Vec`
  protected by `parking_lot::Mutex`.

- **Submitter dispatcher**: `Submitter` enum wraps either `RpcDispatcher` (HTTP
  with round-robin failover across multiple endpoints) or `WsSubmitter` (persistent
  WebSocket connection with per-tx retry). Selected via `--submission-method`.

- **Receipt polling**: Primary confirmation method in burst and sustained modes.
  Concurrent `eth_getTransactionReceipt` for all pending txs (200 concurrent, 25ms
  interval). WS `BlockTracker` serves as backup.

- **Multi-account nonce management**: Default 4 pre-funded validator keys with
  non-overlapping nonce ranges. Per-account txpool slot cap (4000) prevents overflows.

---

## Features

### Execution Modes

- **Burst**: Submit all txs as fast as possible, poll for confirmations
- **Sustained**: Rate-limited submission at target TPS for specified duration
- **Ceiling**: Automatic ramp-up to find maximum sustainable TPS

### Submission Methods

- **HTTP** (default): JSON-RPC batch submission with connection pooling and
  multi-endpoint round-robin failover
- **WebSocket**: Persistent WS connection with exponential backoff retry

### Per-Wave Latency Tracking

Each sender wave gets a wave index. Report includes `per_wave` array with
`{wave, count, p50, p95, p99, max}` per wave for diagnosing txpool pressure.

### Transaction Caching

Pre-signed txs cached to disk as JSON with FNV-1a fingerprinting. Eliminates
signing overhead on repeated runs. API: `cache::save()`, `cache::try_load()`,
`cache::restore_txs()`.

### EVM Workload Generator

Mixed workloads with 7 method selectors matching benchmark contracts:
ERC20 transfer/mint/approve, swap, NFT mint, ETH transfer. Configurable
mix ratios via `EvmMixConfig` with Zipf-like distribution.

### Analytics Pipeline

Post-benchmark bottleneck detection, regression analysis against baseline,
and multi-format reports (ASCII, JSON, Markdown, HTML).

---

## E2E Benchmark Results

| Scenario | Txs   | Confirmed TPS | Latency p50 | Latency p99 |
|----------|-------|---------------|-------------|-------------|
| Small    | 100   | ~170          | <10ms       | <50ms       |
| Medium   | 500   | ~3,740        | ~25ms       | ~100ms      |
| Large    | 2,000 | ~6,400        | ~50ms       | ~200ms      |

---

## Test Coverage

| Area | Unit Tests | E2E Tests |
|------|-----------|-----------|
| Confirmation tracking | 3 | Receipt polling, burst pipeline |
| Submitter dispatcher | 5 | HTTP submit, WS submit, round-robin |
| BlockTracker | 2 | WS block reception |
| Config parsing | 9 | - |
| Transaction caching | 4 | - |
| EVM generator | 7+ | - |
| Analytics pipeline | 3 | - |
| Per-wave latency | 1 | - |
| **Total** | **148+** | **8** |

```bash
# Run unit tests
cargo test -p evm-benchmark

# Run E2E tests (requires a running EVM chain)
cargo test -p evm-benchmark --test e2e_validation -- --ignored --test-threads=1
```

