# BSC Bench Target

Local BSC (BNB Smart Chain) cluster built from source for benchmark comparison.

## Architecture

- **Binary**: BSC geth v1.7.2, built from source (ARM64 + x86_64)
- **Consensus**: Parlia PoSA (Proof of Staked Authority)
- **Validators**: 3 sealing nodes
- **RPC**: 1 dedicated non-sealing node
- **Chain ID**: 714714
- **Block time**: ~3 seconds

## Ports

| Service         | HTTP  | WS    |
|-----------------|-------|-------|
| bsc-rpc         | 8545  | 8546  |
| bsc-validator-1 | 8645  | 8646  |
| bsc-validator-2 | 8745  | 8746  |
| bsc-validator-3 | 8845  | 8846  |

## Pre-funded Accounts

Benchmark accounts (100k BNB each, keys known for bench harness):

| Account | Address | Key derivation |
|---------|---------|----------------|
| Bench 1 | `0x528562E4EA1DFE07B63a6dfC20f8048a9c2E49AB` | `keccak("bsc-bench-1")` |
| Bench 2 | `0x8996D198ae008C81b52Ca95DF26BDabE8cE02684` | `keccak("bsc-bench-2")` |
| Bench 3 | `0x953A381425358C1Abd81D95f9548f242014C7dd4` | `keccak("bsc-bench-3")` |
| Bench 4 | `0x13e77aB15Febd5e3D7dE08e9b402518C436dC69e` | `keccak("bsc-bench-4")` |

Additional pre-funded addresses (100k BNB each, for external testing):
- `0x59b02D4d2F94ea5c55230715a58EBb0b703bCD4B`
- `0x7fd60C817837dCFEFCa6D0A52A44980d12F70C59`
- `0x8E1Ad6FaC6ea5871140594ABEF5b1D503385e936`
- `0xA2bC4Cf857f3D7a22b29c71774B4d8f25cc7edD0`

Validator accounts (also funded):
- Validator 1: `0xd1c268DD16caF43221528AA0B4247F4215720cFa`
- Validator 2: `0x34e3B5C2e476A1613F84d9323371F08d2Ad8e1b1`
- Validator 3: `0x2FB8d6fB92a4Dfa12DDDbf3F84d7120A7CF897D5`

## Usage

```bash
make up        # Build from source, start cluster, connect peers
make bench     # Run default benchmark (10k tx burst)
make status    # Check block heights on all nodes
make down      # Stop cluster and remove volumes
make clean     # Stop + remove Docker images
```
