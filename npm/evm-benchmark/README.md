# @0xdiesis/evm-benchmark

High-performance EVM load testing harness. Chain-agnostic — works with any EVM-compatible network.

## Install

```bash
npm install -g @0xdiesis/evm-benchmark
```

## Quick Start

```bash
# Download chain targets (Docker configs for Diesis, Sonic, BSC, etc.)
evm-benchmark --setup

# Benchmark any RPC endpoint
evm-benchmark --rpc-endpoints http://localhost:8545 --txs 2000 --fund

# Or run without installing
npx @0xdiesis/evm-benchmark --rpc-endpoints http://localhost:8545 --txs 1000 --fund
```

## Features

- **Burst mode** — Submit all transactions as fast as possible, measure peak TPS
- **Sustained mode** — Hold a target TPS, measure latency stability
- **Ceiling mode** — Ramp load until saturation, find max throughput
- **Multi-endpoint** — Round-robin across multiple RPC endpoints
- **Auto-funding** — Deploy a MultiSend contract and batch-fund senders
- **Detailed reports** — JSON output with TPS, latency percentiles, block analysis

## Supported Chains

Includes Docker-based chain targets for local benchmarking:

| Chain | Validators | Consensus |
|-------|-----------|-----------|
| Diesis | 4 | Mysticeti BFT |
| Sonic | 4 | Lachesis aBFT |
| BSC | 3 | Parlia PoSA |
| Avalanche | 5 | Snowman |
| Sei | 4 | Tendermint |
| Cosmos (Evmos) | 1 | CometBFT |

## Documentation

Full documentation at [github.com/0xDiesis/evm-benchmark](https://github.com/0xDiesis/evm-benchmark).
