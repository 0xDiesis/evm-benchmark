# Sei — Benchmark Target

## Architecture

4 Sei validators running Tendermint BFT consensus with parallel EVM execution (OCC).
Sei is a Cosmos SDK chain with an integrated EVM module, meaning EVM transactions
are wrapped in Cosmos SDK transactions and processed through Tendermint consensus
before being executed by the parallel EVM engine.

- **Cosmos chain ID**: `sei`
- **EVM chain ID**: `713714`
- **Consensus**: Tendermint BFT (fast timeouts: 1s propose, 50ms commit)
- **EVM execution**: Optimistic Concurrency Control (OCC) enabled
- **Account funding**: Built with `MOCK_BALANCES=true` -- any EVM account automatically
  has 100 ETH available, no faucet or genesis allocation needed

## Port Mapping

| Service     | Host Port | Container Port | Description          |
|-------------|-----------|----------------|----------------------|
| sei-node-0  | 28545     | 8545           | EVM JSON-RPC (HTTP)  |
| sei-node-0  | 28546     | 8546           | EVM JSON-RPC (WS)    |
| sei-node-0  | 36657     | 26657          | Cosmos Tendermint RPC |
| sei-node-1  | 28547     | 8545           | EVM JSON-RPC (HTTP)  |
| sei-node-2  | 28549     | 8545           | EVM JSON-RPC (HTTP)  |
| sei-node-3  | 28551     | 8545           | EVM JSON-RPC (HTTP)  |

## Quick Start

```bash
# Build images and start the 4-validator cluster
make up

# Check node status and block heights
make status

# Run the benchmark harness
make bench

# Tear down and clean up
make down
```

## Key Differences from Other Chains

- **Cosmos SDK wrapper**: EVM transactions go through Cosmos SDK message handling
  and Tendermint consensus before EVM execution, adding overhead compared to
  native EVM chains.
- **Mock balances**: The `MOCK_BALANCES=true` build tag auto-funds any EVM account
  with 100 ETH, eliminating the need for pre-funded genesis accounts or faucets.
- **OCC parallel execution**: Sei uses optimistic concurrency control for parallel
  EVM transaction execution within a block.
- **Genesis coordination**: Nodes use a shared Docker volume to exchange gentx files
  and assemble a multi-validator genesis. Node 0 acts as the genesis coordinator.

## Pre-funded Test Keys

With `MOCK_BALANCES=true`, any account is automatically funded. However, these
hardhat-standard keys are available if explicit key material is needed:

```
0x57acb95d82739866a5c29e40b0aa2590742ae50425b7dd5b5d279a986370189e
0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d
```
