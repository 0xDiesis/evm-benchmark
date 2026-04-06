# Scroll Bench Target

Standalone Scroll l2geth node for benchmark comparison.

## Architecture

- **Binary**: scrolltech/l2geth:scroll-v5.7.25 (Docker Hub)
- **Consensus**: Clique PoA (single signer, 1-second block period)
- **Nodes**: 1 standalone l2geth (no L1, no prover, no sequencer)
- **Chain ID**: 53077
- **Block time**: ~1 second
- **Gas limit**: 30,000,000
- **Max tx/block**: 100 (default is 44)

## Important Notes

- This is a **standalone l2geth** without L1 settlement, rollup contracts, or ZK prover.
  It measures raw l2geth execution throughput only.
- ZK circuit capacity limits (CCC) are still enforced by l2geth and may throttle
  throughput below the gas limit ceiling. Complex opcodes (KECCAK256, SSTORE, etc.)
  consume more circuit capacity than simple transfers.
- `scrollChainConfig.useZktrie: true` enables the ZK-trie state backend, which
  has different performance characteristics than MPT.

## Ports

| Service       | HTTP  | WS    |
|---------------|-------|-------|
| scroll-l2geth | 48545 | 48546 |

## Pre-funded Accounts

| Account | Address | Key derivation |
|---------|---------|----------------|
| Signer  | `0xE276ae9338be48AA89bD59eD9dCEB0826e863505` | `sha256("scroll-bench-signer")` |
| Bench 1 | `0x11950BC14473845bb68c0a6C6B5c468854aedCBf` | `sha256("scroll-bench-1")` |

Both accounts are pre-funded with a large ETH balance in genesis.

## Usage

```bash
make up        # Start l2geth container
make bench     # Run default benchmark (10k tx burst)
make status    # Check block height
make down      # Stop container and remove volumes
```
