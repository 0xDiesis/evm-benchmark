# Optimism (Supersim) — Benchmark Target

Single-node OP Stack L2 using Supersim for EVM benchmarking.

## Quick Start

```bash
make up        # start supersim (mock L1 + L2, ~10 sec)
make bench     # run burst benchmark (1000 txs, 1 prefunded sender)
make status    # check L2 block height
make down      # tear down
```

## Architecture

- **Supersim** — lightweight mock OP Stack environment (mock L1 + L2 in a single binary)
- **Chain ID**: 901 (Supersim L2 default)
- **Consensus**: instant blocks (no real L1 sequencing overhead)

## Ports

| Service | HTTP RPC | WebSocket |
|---------|----------|-----------|
| L2      | 9545     | 9546      |
| L1 (mock) | 8545   | 8546      |

## Pre-funded Account

Hardhat account #0, pre-funded with 10000 ETH on the L2:

| Account | Private Key |
|---------|-------------|
| #0 | `0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80` |

Use `--fund` to provision additional sender accounts from this key.

## Bench Commands

```bash
# Default burst (1000 txs)
make bench

# Provision additional senders and run with higher parallelism
bash ./run-bench.sh --fund --senders 50 --txs 4000 --batch-size 500

# Sustained load test
make bench-sustained

# Find TPS ceiling
make bench-ceiling

# Custom run
BENCH_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  cargo run -p evm-benchmark --release -- \
  --rpc-endpoints http://127.0.0.1:9545 \
  --ws ws://127.0.0.1:9546 \
  --chain-id 901 \
  --bench-name optimism_supersim \
  --fund \
  --execution burst --txs 4000 --batch-size 500
```

## Cross-Chain Comparison

```bash
# From repo root
make compare CHAINS="optimism diesis" MODE=burst
```

## Notes

- **Raw EVM execution only**: Supersim runs a mock L1, so results measure pure L2 EVM
  throughput without real L1 data posting overhead or blob fee economics.
- **Single node**: No network latency between sequencer/proposer — this is best-case
  execution performance for the OP Stack EVM.
- **Instant blocks**: Supersim produces blocks on demand, so block time is not a bottleneck.
