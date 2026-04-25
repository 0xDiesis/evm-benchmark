# Benchmark Results

All chains run in Docker on the same host with identical resource constraints.
Each chain is benchmarked in isolation — only one chain runs at a time to prevent
resource contention. The harness pre-signs all transactions, then submits them via
batched JSON-RPC calls across all available RPC endpoints (round-robin). Confirmation
is tracked by polling `eth_getTransactionReceipt` for every pending transaction.
TPS is measured as confirmed transactions divided by wall-clock time from first
submission to last confirmation. Latency is measured per-transaction from submission
to receipt.

Geo-latency profiles inject pairwise network delays between validator containers
to simulate geographic distribution (e.g. US-East ↔ Asia-Tokyo = 180ms RTT).
Each node gets different delays to each peer, matching real-world submarine cable
latencies. Latency is verified by pinging between nodes before each geo benchmark run.

## Benchmark Targets

| Chain | Consensus | Nodes | Version |
|-------|-----------|------:|---------|
| Diesis | Mysticeti DAG BFT | 4 validators + 1 filter | latest main |
| Sei | sei-tendermint BFT | 4 validators | v6.4.0 |
| Sonic | Lachesis aBFT (DAG) | 4 validators | v2.1.6 |
| Berachain | CometBFT + beacon-kit | 4 validators + 1 full | v1.3.7 |
| BSC | Parlia (PoSA) | 3 validators + 1 RPC | v1.7.2 |
| Cosmos (Evmos) | CometBFT | 1 validator | v20.0.0 |
| Avalanche | Snowman (C-Chain) | 5 nodes | v1.14.2 |

## Burst Mode (2,000 txs, 200 senders, 8 workers)

### Clean (0ms RTT — local Docker networking)

| Chain | Confirmed TPS | p50 | p95 | p99 | Confirmed |
|-------|-------------:|----:|----:|----:|----------:|
| **Diesis** | **3,114** | **588ms** | **797ms** | **810ms** | 100% |
| Sei | 1,795 | 2,660ms | 4,123ms | 4,570ms | 100% |
| Sonic | 673 | 1,306ms | 1,827ms | 1,839ms | 100% |
| Berachain | 568 | 1,486ms | 3,497ms | 3,502ms | 100% |
| BSC | 242 | 5,251ms | 5,397ms | 8,257ms | 100% |
| Cosmos (Evmos) | 134 | 3,871ms | 4,118ms | 4,444ms | 100% |
| Avalanche | 89 | 10,483ms | 18,518ms | 22,551ms | 100% |

### Geo-US (20-60ms RTT — simulated US-distributed)

| Chain | Confirmed TPS | p50 | p95 | p99 | Confirmed |
|-------|-------------:|----:|----:|----:|----------:|
| **Diesis** | **2,013** | **845ms** | **971ms** | **976ms** | 100% |
| Sei | 1,163 | 2,762ms | 4,232ms | 4,846ms | 100% |
| Sonic | 455 | 1,987ms | 3,802ms | 4,430ms | 100% |
| BSC | 235 | 5,751ms | 5,909ms | 8,472ms | 100% |
| Cosmos (Evmos) | 141 | 3,660ms | 3,905ms | 4,182ms | 100% |

### Geo-Global (60-240ms RTT — US / EU / Asia spread)

| Chain | Confirmed TPS | p50 | p95 | p99 | Confirmed |
|-------|-------------:|----:|----:|----:|----------:|
| Sei | 1,441 | 3,686ms | 5,294ms | 5,579ms | 100% |
| **Diesis** | **985** | **1,810ms** | **1,981ms** | **1,984ms** | 100% |
| Sonic | 368 | 2,980ms | 3,032ms | 4,753ms | 100% |
| BSC | 252 | 5,960ms | 5,977ms | 7,945ms | 100% |
| Cosmos (Evmos) | 158 | 3,618ms | 3,924ms | 4,120ms | 100% |

## TPS Summary

| Chain | Clean | Geo-US | Geo-Global | vs Diesis (clean) |
|-------|------:|-------:|-----------:|------------------:|
| Diesis | 3,114 | 2,013 | 985 | — |
| Sei | 1,795 | 1,163 | 1,441 | 1.7x slower |
| Sonic | 673 | 455 | 368 | 4.6x slower |
| Berachain | 568 | — | — | 5.5x slower |
| BSC | 242 | 235 | 252 | 12.9x slower |
| Cosmos (Evmos) | 134 | 141 | 158 | 23.2x slower |
| Avalanche | 89 | — | — | 35.0x slower |

## Latency Summary (p50)

| Chain | Clean | Geo-US | Geo-Global | vs Diesis (clean) |
|-------|------:|-------:|-----------:|------------------:|
| Diesis | 588ms | 845ms | 1,810ms | — |
| Sonic | 1,306ms | 1,987ms | 2,980ms | 122% higher |
| Berachain | 1,486ms | — | — | 153% higher |
| Sei | 2,660ms | 2,762ms | 3,686ms | 353% higher |
| Cosmos (Evmos) | 3,871ms | 3,660ms | 3,618ms | 559% higher |
| BSC | 5,251ms | 5,751ms | 5,960ms | 793% higher |
| Avalanche | 10,483ms | — | — | 1,683% higher |

## Notes

- All chains confirmed 100% of submitted transactions in all runs.
- Diesis uses the same consensus parameters across all geo profiles (1s block period, 300ms ordering window, 120ms min round delay, propagation threshold 40). This is tuned for geo-global (240ms max RTT) and works safely at all lower-latency profiles.
- Under geo-global latency, Sei achieves higher TPS than Diesis. Sei's tendermint consensus is well-suited for predictable high-latency environments; Diesis's DAG construction overhead is proportionally larger at 240ms RTT.
- Berachain does not have geo-latency topology mappings configured. Berachain uses Kurtosis for orchestration, which requires manual startup.
- BSC uses pre-Luban genesis (no BLS keys) for simplicity. Block time is 3 seconds. BSC geo results show minimal TPS variance (~2%) because Parlia's 3-second block time dominates over the added network latency.
- Cosmos (Evmos) runs as a single-validator node (v20.0.0). The `evmosd testnet start` multi-validator in-process mode has a known bug where the port pool is never populated, making it non-functional in Docker. Geo benchmarks run without network topology simulation (single-node chains have no inter-validator consensus latency). TPS is stable (~134-158) across all geo profiles.
- Avalanche C-Chain block time is ~2 seconds. Only 2,000 txs run over a 5-node cluster.
