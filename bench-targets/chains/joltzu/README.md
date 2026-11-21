# Joltzu local benchmark target

This target starts four Joltzu validators, their development signers, and one
observer. It uses the C++ node's own `tools/devnet.sh` and an explicitly selected
source checkout and data directory. Build the node before starting this target.

## Setup

Use the node's supported GCC 16, CMake 4.4+, Ninja, Conan 2, OpenSSL 3.6.4 and
Boost 1.89.0 toolchain. Follow `cpp-node/docs/devnet.md` and its build instructions.
Keep build output, dependencies, temporary files and network databases in your
configured disposable storage area.

Set these paths in your shell:

```sh
export JOLTZU_REPO_DIR=/path/to/cpp-node
export JOLTZU_BUILD=/path/to/release-binaries
export JOLTZU_DEVNET_DIR=/path/to/dedicated-devnet-data
export OPENSSL=/path/to/openssl-3.6.4/bin/openssl
```

`JOLTZU_DEVNET_DIR` must be separate from the source checkout, with no symlink
components. The launcher records ownership when it initializes a new directory.
It refuses to reset, stop or adopt an existing directory without matching
ownership and configuration. Keep the publicly known funded keys on valueless
local networks only.

## Run

From the benchmark repository root:

```sh
python3 bench-targets/chains/joltzu/devnet.py up
python3 bench-targets/chains/joltzu/devnet.py status

make bench CHAIN=joltzu MODE=sustained TPS=25 DURATION=60 \
  SENDERS=32 WORKERS=4 BATCH_SIZE=4

python3 bench-targets/chains/joltzu/devnet.py down
```

The default observer endpoint is `http://127.0.0.1:30400`, with WebSocket on
the same port, and chain ID 31337. `JOLTZU_BASE_PORT` changes the peer base port
(30300 by default); the observer RPC is at base plus 100. `JOLTZU_CHAIN_ID`
changes the initialized chain ID. `JOLTZU_RPC` and `JOLTZU_WS` can select another
endpoint of that same local network.

Each orchestrated benchmark resets the owned devnet before running. Calling
`up` directly initializes or starts the existing owned network; `down` stops it
while retaining its state, and `reset` deletes and reinitializes that owned state.
The node produces no empty blocks in this profile. Readiness checks RPC first;
workload receipts establish that the chain is making progress.

JSON-RPC batches are capped at 16. The orchestrator lowers larger requested
batches to that limit. Start with four for compatibility with the node's shared
state-read budget. The observer adds a forwarding/following stage; record whether
comparative runs submit through observers or validator endpoints.

## Shared EVM smoke check

The shared probe sends transactions only with either the public Hardhat account
or public development key 1. It checks transfer balances, deploys one Solidity
fixture, writes storage and emits a log, submits a deliberate revert, and verifies
that storage and logs rolled back. Every receipt must match a canonical finalized
block on each selected endpoint.

```sh
export BENCH_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
python3 bench-targets/scripts/evm-smoke.py \
  --rpc http://127.0.0.1:30401 --rpc http://127.0.0.1:30402 \
  --rpc http://127.0.0.1:30403 --rpc http://127.0.0.1:30404 \
  --solc /path/to/solc-0.8.37 --out /path/to/reports/joltzu-smoke.json
```

Stop and start the same devnet, then pass `--verify` with that result and a new
`--out` path to check persisted storage and receipts. Verification sends no new
transactions and checks the original chain ID and genesis hash.

## Comparison scope

Use the same harness revision, workload, sender count, worker count, duration,
batch size and retry policy for each chain. Run chains serially, record their
exact source/image identities, and check successful receipts and canonical
finalized agreement across all validators. A receipt latency measurement alone
does not measure the time at which all validators finalized that transaction.

The separate `joltzu-orderbook` repository measures an engine with advanced
orders and perpetuals. The current node has its own bounded spot implementation;
engine timings and chain throughput describe different execution paths.

The explorer starts with Base Sepolia defaults and needs explicit local RPC,
chain and indexer configuration. The wallet has simulator flows and a separate
opt-in ML-DSA native-transfer profile. Neither app is needed to measure the node.
