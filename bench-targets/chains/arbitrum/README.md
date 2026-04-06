# Arbitrum Nitro — Benchmark Target

L2 optimistic rollup using the official nitro-testnode for EVM benchmarking.

## Quick Start

```bash
make up        # clone nitro-testnode, init + start (~5 min first run)
make bench     # run burst benchmark (1000 txs, 1 prefunded sender + --fund)
make status    # check L2 block height
make down      # tear down
```

## Architecture

- **Arbitrum Nitro** (L2 optimistic rollup)
- **Requires L1**: nitro-testnode runs a local L1 (geth) alongside the L2 sequencer, poster, and validator
- **L2 Sequencer**: single sequencer processes transactions, posts batches to L1
- **Fraud proofs**: optimistic rollup — transactions execute on L2, L1 is used for data availability and dispute resolution
- **Chain ID**: 412346 (nitro-testnode L2 default)

## Ports

| Service | HTTP RPC | WebSocket |
|---------|----------|-----------|
| L2 Sequencer | 8547 | 8548 |
| L1 (geth) | 8545 | 8546 |

## Pre-funded Accounts

From nitro-testnode — the "l2owner" dev account, pre-funded with ETH on L2:

| Account | Private Key |
|---------|-------------|
| l2owner | `0xb6b15c8cb491557369f3c7d2c287b053eb229daa9c22138887752191c9520659` |

Only 1 account is pre-funded. Use `--fund` to provision additional sender accounts from this key.

## Bench Commands

```bash
# Default burst (1000 txs, auto-funds additional senders)
make bench

# Sustained load test
make bench-sustained

# Find TPS ceiling
make bench-ceiling

# Custom run with more senders (--fund provisions them from the pre-funded key)
bash ./run-bench.sh --fund --senders 50 --txs 4000 --batch-size 500

# Custom run
BENCH_KEY=0xb6b15c8cb491557369f3c7d2c287b053eb229daa9c22138887752191c9520659 \
  cargo run -p evm-benchmark --release -- \
  --rpc-endpoints http://127.0.0.1:8547 \
  --ws ws://127.0.0.1:8548 \
  --chain-id 412346 \
  --bench-name arbitrum_nitro \
  --execution burst --txs 4000 --batch-size 500 --fund
```

## Cross-Chain Comparison

Use the top-level comparison scripts to benchmark Arbitrum against other chains:

```bash
# From repo root
make compare CHAINS="arbitrum diesis" MODE=burst
```

## Notes

- **L2 rollup**: Unlike L1 chains, Arbitrum has a single sequencer — there is no multi-validator consensus on L2
- **L1 dependency**: The testnode runs both L1 and L2; L1 must be healthy for L2 to function
- **Single pre-funded key**: Only 1 dev account has L2 ETH by default; use `--fund` to spread funds across multiple senders
- **Port conflicts**: L1 uses ports 8545/8546, L2 uses 8547/8548 — ensure these are free before starting
