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

## Metrics

With a node build that supports `devnet.sh start --prometheus`, set
`JOLTZU_PROMETHEUS=1` for a fresh run. The launcher enables loopback metrics on
all five processes. The orchestrator writes `joltzu-metrics-before.json` and
`joltzu-metrics-after.json` in the run directory. Each snapshot records each
node's finalized height, mailbox drops, send timeouts, and any available signer
and durable-commit timing counters. Failed scrapes are recorded per node and do
not interrupt the workload.

Run `python3 bench-targets/chains/joltzu/metrics.py --out /path/to/metrics.json`
to sample a running devnet directly. Compare counter differences on the same
node process. A restart, including a ceiling run that restarts between steps,
resets the counters. Heights in the snapshots describe observed progress.
Use the receipt and canonical-block checks below to establish finality.

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

## Isolated memory experiment

Use Docker with cgroup v2 and a local Linux image containing Bash, Python 3,
curl, ps, GNU timeout, and OpenSSL 3. Supply prebuilt node binaries, solc 0.8.37,
and Foundry cast for the image's architecture. The runnable toolchain image from
`cpp-node/deploy/docker/Dockerfile` supplies the system tools.

```sh
python3 bench-targets/chains/joltzu/memory-check.py \
  --image joltzu-toolchain --node-repo "$JOLTZU_REPO_DIR" \
  --node-build "$JOLTZU_LINUX_BUILD" --solc "$LINUX_SOLC" --cast "$LINUX_CAST" \
  --baseline-mib 1024 --pressure-mib "$PRESSURE_MIB" --cpus 2 \
  --out /path/to/new-memory-run
```

The script runs the same workload twice in separate containers with no external
network and no swap, then verifies the receipts and stored state after restarting
each devnet. The baseline must stay below its cap. Choose the lower cap based
on the baseline's measured peak. The pressure run succeeds only when the kernel
records a cap event, no out-of-memory event occurs, and the smoke and restart
checks pass. If Docker cannot confirm removal, the script records the container
name and cleanup error in the profile's `console.log` and on standard error.

Each `cpu_usec` sample is cumulative CPU time in microseconds for the entire
container cgroup. Subtract consecutive samples to find the CPU time between
them. Each `peak_bytes` sample is the highest memory usage since the cgroup was
created, through the time of that sample. It is `null` when the kernel does not
expose `memory.peak`. Each stage reports its own wall-clock `elapsed_seconds`.

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
