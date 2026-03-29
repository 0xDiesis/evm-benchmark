# Evmos (Cosmos + EVM) Bench Target

Benchmarks against a local [Evmos](https://github.com/evmos/evmos) network.
Evmos runs the EVM inside the Cosmos SDK via Ethermint, providing full
Ethereum JSON-RPC compatibility on top of Tendermint BFT consensus.

## Architecture

- **Consensus**: Tendermint BFT (CometBFT)
- **EVM layer**: Ethermint (Cosmos SDK module)
- **Validators**: 4-node localnet (default)
- **Chain ID**: 9000 (Evmos localnet default)
- **Block time**: ~1-2 seconds

## Ports

| Service       | Port  |
|---------------|-------|
| JSON-RPC HTTP | 8545  |
| WebSocket     | 8546  |

## Usage

```bash
make up        # Clone, build, and start Evmos localnet
make bench     # Run benchmark suite
make status    # Check current block height
make down      # Stop localnet
```
