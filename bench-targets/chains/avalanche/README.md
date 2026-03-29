# Avalanche C-Chain Benchmark Target

5-node local Avalanche network for EVM benchmarking via the C-Chain (coreth).

## Architecture

- **Nodes**: 5 validators running avalanchego v1.14.1
- **Consensus**: Snowman (linear chain consensus for C-Chain)
- **EVM**: C-Chain powered by coreth, built into avalanchego
- **Network ID**: local (12345)
- **C-Chain ID**: 43112
- **Sybil protection**: disabled (local testnet)
- **Staking certs**: ephemeral (auto-generated per run)

## Pre-funded Account

- **Address**: `0x8db97C7cEcE249c2b98bDC0226Cc4C2A57BF52FC`
- **Private key**: `56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027`
- **Balance**: ~10^30 wei (effectively unlimited)

## Port Mapping

| Node   | Host Port | RPC Endpoint                              |
|--------|-----------|-------------------------------------------|
| avax-1 | 9650      | http://localhost:9650/ext/bc/C/rpc        |
| avax-2 | 9660      | http://localhost:9660/ext/bc/C/rpc        |
| avax-3 | 9670      | http://localhost:9670/ext/bc/C/rpc        |
| avax-4 | 9680      | http://localhost:9680/ext/bc/C/rpc        |
| avax-5 | 9690      | http://localhost:9690/ext/bc/C/rpc        |

WebSocket: `ws://localhost:9650/ext/bc/C/ws`

## Quick Start

```bash
make up      # Build images and start 5-node network
make status  # Check node health and C-Chain block heights
make bench   # Run evm-benchmark against the network
make down    # Tear down and remove volumes
```

## Network Topology

Node 1 (avax-1) serves as the bootstrap node on subnet `10.0.30.0/24`.
Nodes 2-5 bootstrap from node 1's IP and join the local network automatically.
All nodes use ephemeral staking certificates with sybil protection disabled.
