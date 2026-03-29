# Sonic Fakenet — Benchmark Target

4-node Sonic validator testnet for EVM benchmarking.

## Quick Start

```bash
make up        # build from source, start 4 nodes, connect peers (~5 min first build)
make bench     # run burst benchmark (1000 txs, 4 prefunded senders)
make status    # check block heights
make down      # tear down
```

## Architecture

- **Sonic v2.1.6** (Go, built from source)
- **4 validators** using `--fakenet N/4` (DAG-based Lachesis BFT, quorum=3)
- **Chain ID**: 4003 (fakenet default, from `example-genesis.json`)
- **Min base fee**: 50 GWei
- **Block gas limit**: 5,000,000,000 (5B)

## Ports

| Node | HTTP RPC | WebSocket | P2P |
|------|----------|-----------|-----|
| sonic-1 | 18545 | 18546 | 5050 |
| sonic-2 | 18645 | 18646 | 5050 |
| sonic-3 | 18745 | 18746 | 5050 |
| sonic-4 | 18845 | 18846 | 5050 |

## Pre-funded Validator Keys

From `evmcore/apply_fake_genesis.go` — all auto-funded with ~10^9 S tokens:

| Validator | Private Key |
|-----------|-------------|
| 1 | `0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7` |
| 2 | `0x3144c0aa4ced56dc15c79b045bc5559a5ac9363d98db6df321fe3847a103740f` |
| 3 | `0x04a531f967898df5dbe223b67989b248e23c1c356a3f6717775cccb7fe53482c` |
| 4 | `0x00ca81d4fe11c23fae8b5e5b06f9fe952c99ca46abaec8bda70a678cd0314dde` |

## Bench Commands

```bash
# Default burst (1000 txs, all 4 RPC endpoints)
make bench

# Provision additional sender accounts, then run with a higher sender count
make fund BENCH_SENDERS=200
bash ./run-bench.sh --fund --senders 200 --txs 4000 --batch-size 500

# Sustained load test
make bench-sustained

# Find TPS ceiling
make bench-ceiling

# Custom run
BENCH_KEY=0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7 \
  cargo run -p evm-benchmark --release -- \
  --rpc-endpoints http://localhost:18545,http://localhost:18645,http://localhost:18745,http://localhost:18845 \
  --ws ws://localhost:18546 \
  --chain-id 4003 \
  --bench-name sonic_fakenet \
  --execution burst --txs 4000 --batch-size 500
```

## Cross-Chain Comparison

Use the top-level comparison scripts to benchmark Sonic against other chains:

```bash
# From repo root
make compare CHAINS="sonic sei" MODE=burst
```

## Notes

- **No fixed block time**: Sonic uses DAG consensus — events finalize asynchronously
- **Higher min base fee**: 50 GWei — gas costs are higher than some chains
- **Peer discovery**: Requires explicit `admin.addPeer` after startup (handled by `connect-peers.sh`)
