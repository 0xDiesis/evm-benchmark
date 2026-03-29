# Berachain — Benchmark Target

## Architecture

4 Berachain validators + 1 full node running via Kurtosis (beacon-kit).
Berachain uses a two-client architecture similar to post-merge Ethereum:

- **Consensus client**: BeaconKit (`beacond`), built on CometBFT
- **Execution client**: bera-reth (Reth fork)
- Communication via Engine API

- **Chain ID**: `80087`
- **Consensus**: CometBFT single-slot finality (~2s block time)
- **Block gas limit**: 30,000,000

## Prerequisites

- [Kurtosis CLI](https://docs.kurtosis.com/install/) v0.90.1+
- Docker v25+
- ~8 GB RAM free (5 EL + 5 CL containers)

## Port Mapping

Kurtosis assigns dynamic host ports. After `make up`, ports are discovered
automatically and written to `.env`. Run `make status` to see current mappings.

## Quick Start

```bash
# Install Kurtosis if needed
brew install kurtosis-tech/tap/kurtosis-cli

# Clone beacon-kit, build images, start 4-validator devnet
make up

# Check node status and block heights
make status

# Run the benchmark harness
make bench

# Tear down
make down

# Full cleanup (removes cloned beacon-kit repo)
make clean
```

## Pre-funded Test Keys

From beacon-kit Kurtosis constants (100 accounts total). Primary accounts:

| # | Address | Balance |
|---|---------|---------|
| 0 | `0x20f33ce90a13a4b5e7697e3544c3083b8f8a51d4` | ~10^30 wei |
| 1 | `0x56898d1aFb10cad584961eb96AcD476C6826e41E` | ~86k BERA |
| 2 | `0x1e2e53c2451d0f9ED4B7952991BE0c95165D5c01` | ~86k BERA |
| 3 | `0x3bd0E8f1B1E8Ec99a4E1762F4058F9884C93af31` | ~86k BERA |

Keys are in `run-bench.sh`. The harness uses `--fund` to redistribute from
these accounts to ephemeral senders.

## Key Differences from Other Chains

- **Two-client architecture**: Unlike Sonic (monolithic) or Sei (Cosmos SDK + EVM module),
  Berachain runs separate CL + EL processes communicating via Engine API.
- **CometBFT consensus**: ~2s block time with single-slot finality. Slower than
  Diesis (200ms) or Sonic (sub-second DAG), comparable to Sei (~1-3s).
- **Kurtosis orchestration**: Uses Kurtosis instead of raw Docker Compose, which
  adds a dependency but provides better multi-service orchestration.
- **Dynamic ports**: Unlike other bench targets with fixed port mappings, Kurtosis
  assigns random host ports. The `discover-ports.sh` script handles this.
