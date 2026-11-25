# Diesis benchmark methodology

All Diesis load generation and speed measurement runs through this repository. This page
covers:
- the official procedure;
- what the Diesis target adds to the generic harness;
- every reported metric.

## The official benchmark

EVM targets are judged under the **`capped`** profile:
- 4 validators, each with 2 pinned CPUs and 4 GiB, logging to file at info;
- 600 s measured windows (after 60 s of warmup);
- a chain aged past 5,000 blocks;
- interleaved A B B A arms, at least 3 per side;
- medians and ranges reported.

**Workload:** the judged mix (below), under an explicitly pinned genesis outcome signature rule.

**Primary results:**
- finalized TPS, counting successful receipts only;
- **submit→finalized** p50/p95/p99, with targets of 1.5 s, 2.5 s, and 4 s;
- **submit→receipt** p50/p95/p99.

**Secondary:** latest→finalized, then the per-block and resource attribution below.

The first throughput milestone is at least 2,000 successful finalized TPS on the judged mix.
Use block intervals, follower lag, and phase timings to identify the largest cost and choose
the next change. Compare it on the same aged workload with matching node settings and host
conditions.

`uncapped` is for diagnosis only. Both profiles log to file at info (see File log level).

Judged runs run alone. `judged.py` starts its fleet only while no other Diesis fleet runs, and a
window with another fleet up at its start or end is marked functional-only.

Start with a current-main baseline using the normal node settings. Use a compatible saved
fixture or age the fleet in place. Record the revision, image, and first validator error if
startup fails. If the judged run exposes resource pressure, use that run to decide what to
investigate and compare the fix under the same validator limits.

```bash
# Image for the node commit under test (heavy: run through the compute broker).
# The checkout must be clean and contracts/out a real directory (see build-image.sh).
compute run --class heavy --where local -- bash bench-targets/chains/diesis/build-image.sh <node-checkout>

# Age a fleet in place with the capped profile when a compatible fixture is unavailable:
BENCH_DIESIS_PROFILE=capped make diesis-age TAG=main-5k DIESIS_REPO_DIR=<node-checkout>

# One aged baseline using the pinned genesis outcome rule:
make diesis-judged TAG=main-5k RATE=2000 OUTCOME=bls-only

# A/B of a change against main at the judged profile:
make ab NAME=<change> AGED=main-5k RATES=2000 \
  A="profile=capped,image=<main-sha>-release,repo=<main-checkout>" \
  B="profile=capped,image=<candidate-sha>-release,repo=<candidate-checkout>,name=<change>"

# Save a stopped aged fleet as a fixture, then the judged run on a fresh copy of it
# (OUTCOME must match the rule the fixture was built with; MIX=judged is the default):
make diesis-fixture-save TAG=main-5k
make diesis-judged FIXTURE=<fixture> RATE=2000 OUTCOME=bls-only

# The node's built speed flags all off against all on, A B B A (FIXTURE= or AGED=, or fresh):
make diesis-speed-flags NAME=<name> FIXTURE=<fixture> RATES=2000

# Idle CPU, cadence and resume latency per DIESIS_IDLE_STRETCH, interleaved 4 1 1 4:
make diesis-idle STRETCH=4,1 ARMS=2

# One ad-hoc run (fresh genesis unless AGED is set); MIX=judged drives the judged mix:
make bench CHAIN=diesis MODE=sustained TPS=2000 DURATION=660 WARMUP=60 PROFILE=capped MIX=judged
```

## The judged mix

`--mix judged` (bench.sh, `make bench MIX=judged`, `judged.py` by default) drives the pinned
mixed-EVM workload in `crates/evm-benchmark/src/generators/judged_mix.rs`. It runs only in
sustained mode. The generator's default EVM mix (mint-heavy, no order book) is not the judged mix.

**Proportions, by transaction.** Each seeded block of 100 transactions holds them exactly:

| Type | Share | Call |
|---|---|---|
| `ETHTransfer` | 25% | native transfer, 1-1,000 wei |
| `ERC20Transfer` | 30% | `BenchmarkToken.transfer`, 1-1,000 units |
| `ERC20Approve` | 10% | `approve(spender, 1000 + plan index)`: every approval changes the allowance |
| `ERC20Mint` | 5% | `mint(recipient, 1-1e6)` |
| `NFTMint` | 5% | `BenchmarkNFT.mint(recipient)` |
| `SpotPlace` | 15% | resting 1-lot `submitLimitOrder` inside the active band |
| `SpotCancel` | 5% | `cancelOrder` of a live seeded order, signed by its owner |
| `SpotTake` | 5% | 2-lot crossing `submitLimitOrder` that fills two distinct 1-lot makers |

Order-book calls go straight from the trader's account to `DiesisSpotBook`; a crossing order
matches inside that call. The plan's success is defined by finalized receipts, so a taker
counts once however many makers it fills.

**Pinned contention.**
- **Plan:** seed `0x4a55444745444d58`, SplitMix64, so a plan is identical on every host.
- **Senders:** 400, round-robin within role: 300 general, 80 makers (every placement and every
  cancel) and 20 takers. Takers never fill their own orders. The first four are the funding
  keys; key 0x01 must be first, because it is the development governance owner and holds
  TEST-USD and stDS.
- **Contracts:** 8 ERC-20 contracts and 4 NFT contracts, each chosen with Zipf exponent 1.0.
- **Recipients:** a fixed pool of 4,096 addresses with Zipf exponent 0.8.
- **Approval spenders:** 64, chosen uniformly.
- **Markets:** WDS/TEST-USD 40%, stDS/TEST-USD 35%, stDS/WDS 25%. These are pricing-version-0
  markets, the only kind direct `submitLimitOrder` accepts, so there is one per ordered token pair.
- **Book, in raw ticks around mid 1,000:**
  - placements rest within 10 levels of the mid;
  - takers cross the whole active side (buy at mid+10, sell at mid-10);
  - cancel targets rest 51-100 ticks away, beyond every taker limit.
- **Seeding:** 64 active orders per side per market are seeded before the window, plus one deep
  order for every cancel the plan holds. The plan refuses to start if any market would pass the
  node's 32,768 active orders per market. A judged 660 s run at 2,000 TPS peaks near 29,000 in
  the 40% market.
- **Declared gas, fixed per type:**

  | Type | Declared gas | Basis |
  |---|---|---|
  | native transfer | 21,000 | the exact intrinsic cost |
  | ERC-20 transfer | 65,000 | harness per-call limit; worst case writes a cold zero recipient balance |
  | approve | 60,000 | harness per-call limit; worst case writes a cold zero allowance |
  | ERC-20 mint | 80,000 | harness per-call limit; worst case writes a cold zero balance and `totalSupply` |
  | NFT mint | 150,000 | harness per-call limit; writes a new owner slot, `totalSupply` and possibly a zero balance |
  | `SpotPlace` | 115,960 | worst-case calldata intrinsic + the node's 1-lot matching precharge |
  | `SpotTake` | 151,408 | worst-case calldata intrinsic + the 2-lot precharge |
  | `SpotCancel` | 24,288 | worst-case calldata intrinsic + the flat 3,000 cancel charge |

  The five EVM-call limits are the generator's existing per-call constants (`evm_mix.rs`), which
  predate the judged mix and were not tuned to the block. The spot-book limits are minimums: the
  node charges the precharge before any work, and on a live fleet one gas less reverted out of gas.
  Together that is 68.4M declared gas per 1,000 transactions. A 4-validator smoke measured 42.2M
  gas used per 1,000 successful transactions.

**Setup before the window** (not measured):
1. Deploy the contracts.
2. Fund the senders with native DS.
3. Create the markets, skipping any that already exist.
4. Send each order-book trader TEST-USD and stDS, then have each trader wrap DS and deposit WDS,
   stDS and TEST-USD into settlement.
5. Mint ERC-20 holdings to the general senders.
6. Seed the books, reading the cancel targets' order ids from their `OrderPlaced` logs.

Setup transactions must all succeed. `judged-mix.json` beside `report.json` records:
- the pinned parameters;
- the planned counts per type;
- the seeded book sizes;
- the setup time.

**Outcome rule.** `judged.py --outcome-rule bls-only|hybrid` is required. A rule that differs
from the fixture's genesis is refused. The rule is written to `judged.json` and `judged.md`.

**Validity.** A judged run is invalid when fewer takers filled two distinct makers than succeeded:
a taker that matches fewer makers still returns status 1, so success alone would hide a drained book.
The observer reports `taker_fills` (successful takers, how many filled two distinct makers, fills per
taker) on every run and adds the shortfall to `invalid_reasons`; `judged.json`/`judged.md` carry
`valid`, the reasons and the fills either way.

**File log level.** Validators' reth file log (`--log.file.filter`) runs at info by default:
reth's own default, debug, wrote 61 MB in 9 idle minutes on one node, keeps debug-gated probe work
running and inflates CPU figures, and its mismatch with stdout at info makes stdout drop lines. Both
profiles set `FLEET_FILE_LOG_FILTER=info`, so capped and uncapped differ only in capacity, and
`judged.py` sets `BENCH_DIESIS_FILE_LOG_FILTER=info` explicitly, then refuses a fleet that records any
other level. Debug is an explicit override for diagnostic arms: `BENCH_DIESIS_FILE_LOG_FILTER=debug`,
which wins over the profile, or `filelog=debug` on an `ab.py` side (default `filelog=info`; `+`
separates several directives). `ab.py` records each side's level in `plan.json` and the levels its
steps reported in `summary.json`/`summary.md`.
The level is recorded in the fleet provenance, in `judged.json`/`judged.md`, and as
`chain_config.validator_file_log_filter` in each run's `meta.json`.
Timeline marks are info events, so they still land in the file log at info: a 4-validator check with
all three timelines on kept every `decided`, `installed`, `block_cut`, sampled `tx_admitted` and vote
mark, with no debug lines. With stdout and the file both at info, stdout no longer drops marks either.

On the shared M2, do not reserve the host: there is no `compute measure begin --host local`,
and development comes first. Before each arm, `ab.py` waits up to 10 minutes for 60 s of
quiet (1-minute load below 8, no compiler or docker build) and records the outcome in
`arm-*/host-*.json`. The observer records the host load for every window. Treat arms run on
a busy host as diagnostic. Set `BENCH_RESULTS_BASE` to keep results off the system disk
(in this workspace, `/Volumes/Scratch/0xdiesis/reports/evm-benchmark`). Set
`BENCH_DIESIS_MAX_FLEETS=2` to honour a shared fleet cap. Before starting any fleet, including
each arm's restart, `fleet.py` then waits while two or more other Diesis fleets run on the host.
It counts every user's fleets, one per compose project, matched by image ID. To hand a running
series' slot to another fleet, create the file named by `BENCH_DIESIS_HOLD_FILE`. The next arm
tears its fleet down and waits until the file is removed.

## What the harness already provided

| Area | Existing capability |
|---|---|
| Execution modes | burst (waves), sustained (pre-signed, paced, `target hit` at 95%), ceiling (adaptive step search, saturation confidence score, restart between steps) |
| Environments | `ENV=clean` and eight `geo-*` tc-netem layouts with topology verification and geo-safe consensus timing |
| Ingress | Diesis RPC limiter kept on but raised (`BENCH_DIESIS_RATE_LIMIT_*`), HTTP or WebSocket submission, multi-endpoint round robin with failover, retry profiles |
| Accounting | attempted / accepted / failed / confirmed / pending, `valid` and `invalid_reason`, confirmed %, per-wave latency, finality-confirmation depth mode, cost metrics, preflight. `confirmed` counts included transactions that did not revert; `successful`, `reverted` and `status_unknown` (inclusions whose receipt status was never read) are reported apart, per method too |
| Confirmation | recent-block scans, bounded receipt polling, WebSocket `newHeads`; strict hash matching by default |
| Analytics | bottleneck detection, regression against baselines, recommendations; ASCII, JSON, Markdown and HTML reports |
| Chain diagnostics | Prometheus before/after deltas through the e2e Prometheus (`prometheus-diagnostics.py`: DAG drops, payload repair, commit skips, tx-source selection) |
| Comparisons | `make compare` across chains, `make sweep` over eight Diesis parameters, a results index, ledger and ad-hoc `results-compare`, and `meta.json` provenance per run |
| Observer proxy | `latency-proxy.py`: receipt and finalized availability through an HTTP proxy. It suits low-concurrency cross-chain comparisons; its in-flight bound makes it unsuitable for Diesis capacity runs |

Several things in the old `.work-in-progress` harness duplicated this: generator invocation,
funding, Prometheus snapshots, run directories and result indexing. The old harness also
lacked geo profiles, ceiling search, analytics, cross-chain comparison and sweeps.

## What the Diesis target adds

| Gap | Added |
|---|---|
| Fleet from a node image or commit | `chains/diesis/fleet.py` renders the node checkout's `docker-compose.e2e.yml` plus `docker-compose.mpt-signing.yml` for an existing `diesis-node-e2e:<tag>` (default `<checkout HEAD>-release`). It never builds; `build-image.sh` does |
| Resource profiles | `chains/diesis/profiles/capped.env` (the run rule) and `uncapped.env`; CPU, cpuset and memory limits are applied to the rendered compose |
| Aged-chain reuse | `fleet.py age`; `AGED=<tag>` restarts the stopped aged fleet on its own volumes |
| Per-transaction latency | The harness logs each accepted hash with its submit time (`BENCH_SUBMIT_LOG`). `diesis-observer.py` joins that log with node 1's block trace and the all-four finalized head |
| Diesis attribution | Per-node Prometheus deltas: selected-owner hold, MPT quorum wait, rounds/block, tx/block, interval; also empty-block share, CPU-seconds per tx (Docker), pool backlog slope |
| Env and round timer | `BENCH_DIESIS_ENV="K=V ..."` for any node flag, or the node-Makefile `E2E_*` spellings (for example `E2E_MIN_ROUND_DELAY=20ms`). Ambient `DIESIS_*` values only feed the compose's own interpolation and are never injected: the node's strict configuration rejects any `DIESIS_*` key it does not consume, and workspace tooling exports such names |
| Interleaved A/B | `ab.py` / `make ab`: A B B A …, medians and ranges, resumable |
| Fixtures and the judged run | `fleet.py save-fixture` archives a stopped aged fleet; `judged.py` restores a fresh copy, records a refusal (with Work-admission bytes) or one 600 s window, and never modifies the fixture |
| Speed flags | `make diesis-speed-flags`: the node's built speed flags all off against all on, through `ab.py` |
| Idle stretch | `diesis-idle.py` / `make diesis-idle`: per `DIESIS_IDLE_STRETCH` arm, CPU % per validator over an idle window, blocks and consensus rounds per second, idle block intervals, and submit→first block, receipt and finalized for single transfers after long idle gaps, with every validator's idle-mode exit, head progress and hash agreement |
| Resource and attribution columns | Per validator: CPU seconds per transaction and per consensus round, CFS throttled seconds per second and throttled-period share (Docker stats; nonzero only when a quota binds), owner hold per block by site, duplicate proposals, output deferrals and fallback entries per block (fallback by cause and resolution path), largest block, validator head spread, and follower lag when `DIESIS_FOLLOWER_RPC` names follower endpoints (`follower_present` says whether a run had one). Succeeded and reverted counts come from the one post-drain receipt read behind `outcomes` (see `finalized_tps`) |
| Round timeline | With `DIESIS_TRACE_ROUND_TIMELINE=true`, `DIESIS_TRACE_TX_SAMPLE=N` and `DIESIS_TRACE_MPT_VOTE_TIMELINE=true` on the validators, the observer copies their timeline lines to `timeline-<node>.log` and writes `timeline-blocks.json`; `diesis-timeline.py analyze --run-dir DIR` splits sampled transactions into the stages submit→carrying vertex, →leader vertex, →round L+3, →decided, →installed, →block cut, →vote requested, →quorum, →served, →finalized, and splits the inclusion wait by whether the submit node owned the sender's partition. `diesis-timeline.py probe` runs idle inclusion probes over all four RPC endpoints on a fresh fleet |
| Load mode | `ab.py` sides take `test=transfer|evm`; `evm` is the generator's default mix (mint-heavy, no order-book traffic), not a judged mix |
| Verdicts | `generator_bound` and `chain_saturated` per run |

Fixes made along the way:
- Concurrent sustained submissions now spread across all four RPC ports. They used to
  converge on one.
- `BENCH_PRESIGN_HEADROOM` sizes the pre-signed pool (`bench.sh` uses 1.1). The fixed 5×
  pool took minutes and gigabytes on long high-rate runs.
- The Prometheus URL defaults to 19090, where the e2e compose publishes it.
- Sustained Diesis runs default to 256 workers. The Makefile no longer forces 8, which caps
  delivery near workers ÷ RTT.
- The Diesis target no longer tags images by branch name or builds a dev image implicitly.

## Fleet

- **Render.** `docker compose config --format json` runs on the node checkout with the
  profile and overrides in the environment. The result is then patched:
  - only node-1..4 and Prometheus are kept;
  - the image is pinned to its immutable ID;
  - host ports are bound to 127.0.0.1;
  - the profile's limits are applied;
  - the merged environment is set on every validator.

  The rendered compose and its provenance (checkout HEAD, image ID, profile, validator
  environment) go to `$BENCH_RESULTS_BASE/fleet/<project>/`. `bench.sh` copies the
  provenance to `fleet-provenance.json` in each run directory.
- **Outcome signature rule.** Fleets run a BLS-only genesis by default
  (`BENCH_DIESIS_OUTCOME_RULE=bls-only`). Select `hybrid` to measure a development fleet
  whose node revision signs Horizon outcomes with both BLS and ML-DSA-65. The fleet sets
  `DIESIS_DEV_OUTCOME_SIGNATURE_RULE` on every validator and selects the matching epoch-zero
  artifact for BLS-only. An aged fleet keeps its genesis rule, which is recorded in the run
  provenance. `ab.py` takes `outcome=hybrid|bls-only` per side. Use compatible fixtures for
  each rule and record the node revision and image before comparing results. The current
  executor-packing fixture is BLS-only. On checkouts that predate the rule control, leave
  `BENCH_DIESIS_OUTCOME_RULE` unset.
- **Port blocks.** Each fleet takes the first free block: block k adds 200·k to the published
  RPC (18545–18575), WS (18546), metrics (19101–19104) and Prometheus (19090) ports, and owns
  subnet `10.100.(100+k).0/24`. A port is busy when anything listens on it or Docker publishes
  it; a subnet is busy when any Docker network uses it. A project keeps its previous block
  while that block stays free. P2P ports stay inside each fleet's network.
  - The block, container names (`<project>-node-N`) and node IPs go to
    `$BENCH_RESULTS_BASE/fleet/<project>/ports.json` and the provenance that `bench.sh`
    stores with every run.
  - `lib.sh` routes RPC, WS, metrics, Prometheus and the topology container names and IPs from
    that record, so concurrent fleets never collide.
  - Concurrent users on one host set distinct `BENCH_DIESIS_FLEET` names for their fresh
    fleets. Starting a fleet stops only the same user's other fleet.
  - `down` removes only that user's fresh project.
- **CPU pinning.** The capped profile pins validator i to 2 consecutive CPUs from a base, so a
  lone capped fleet keeps CPUs 0–7 and its results stay comparable. A capped fleet that starts
  while another Diesis fleet pins CPUs takes whichever of base 0 and (Docker CPUs − 8) overlaps
  those CPUs least: CPUs 4–11 on the M2's 12-CPU Docker VM. Two capped fleets there still share
  CPUs 4–7, so concurrent fleets are functional runs only. The base and each validator's cpuset
  go to the provenance, which `bench.sh` copies into every run directory.
- **Aged fleets.** Copying volumes does not work: each validator binds its writer-fence
  journal to its datadir's device and inode (diesis-reth
  `crates/engine/tree/src/writer_fence.rs`), and a copy is refused at startup. An aged fleet
  is therefore one stopped compose project, `diesis-bench-aged-<tag>`. Every run re-renders
  it for its own image, profile and overrides, then starts it on the same volumes.
  - `down` only stops it; `fleet.py destroy-aged --tag` removes it.
  - The chain keeps aging from run to run, and A B B A ordering balances that drift.
  - Re-age after a schema, genesis or validator-key change.

## Metrics (`diesis-metrics.json` per sustained run)

The measured window is `[load start + warmup, load start + duration]`. Load start is the
first submission in the submit log; the observer and generator share the host clock.

| Field | Definition |
|---|---|
| `finalized_tps` | Successful transactions (receipt status 1) in blocks whose all-four `finalized` head passed them inside the window, ÷ window. After the drain the observer reads `eth_getBlockReceipts` for the run's blocks; `finalized_basis` is `successful`, or `included` when receipts were not read (`--no-receipts`) |
| `finalized_included_tps`, `outcomes` | Every included transaction in those blocks ÷ window; successful, reverted and status-unknown counts among them |
| `reverted_share` | Window submissions that reverted; reverted transactions are excluded from the latency populations below |
| `per_type` | Per submit-log method: successful finalized TPS and share of successes, reverts, submit→finalized and submit→receipt p50/p95/p99, mean gas used (successful and reverted receipts), and spot-book fills per successful transaction (`two_distinct_makers` for takers) |
| `taker_fills`, `invalid_reasons` | Successful `SpotTake` transactions, how many filled two distinct makers, and fills per taker; a shortfall is an invalid reason (judged validity, above) |
| `submit_to_finalized_s` p50/p95/p99 | Every transaction submitted inside the window that did not revert: time from submission until all four validators' finalized head reached its block. Transactions never included, or not finalized by the end of the drain (`BENCH_DIESIS_DRAIN_SECS`, default 150), count as infinite, so a percentile that lands on them is `null`; `unfinalized_share` gives their share |
| `submit_to_receipt_s` p50/p95/p99 | The same population, to its block's first appearance on node 1 (50 ms poll) |
| `latest_to_finalized_s` | Per block in the window, finalized time minus first-seen time, weighted by transactions |
| `committed_tps`, `all_four_latest_tps` | Same as finalized TPS, by node 1 first-seen time and by the time all four reached the height (the Nov 8-9 basis) |
| `block_interval_ms` p50/p99 | Consecutive first-seen times on node 1, quantized to about 50 ms |
| `per_block_median` | Window deltas of each validator's Prometheus, median of four: tx/block, interval, `rounds_per_block` (Δ`consensus_current_round` ÷ Δblocks), selected-owner hold ms/block, MPT quorum wait ms/block. Bounds are the 5 s samples nearest the window edges (`bounds_s`) |
| `hold_ms_per_block_5s`, `quorum_wait_ms_per_block_5s` | p50/p95/p99 over the window's 5 s intervals on node 1 of the per-block mean |
| `quorum_wait_ms_exporter` | The exporter's rolling per-certification quantiles, median over the window |
| `cpu_s_per_tx_nodes`, `cpu_s_per_tx_median` | Container CPU-seconds over the window ÷ finalized transactions |
| `pool_slope_tx_per_s`, `pool_full_at_s` | Linear slope of node 1's pending pool, up to the moment it reached 98% of its 50k cap |
| `empty_block_share` | Blocks first seen in the window with no transactions |
| `generator_bound` | Attempted below 95% of target, the same threshold as the harness's `target hit`. The run measures the client, not the chain |
| `chain_saturated` | Finalized below 95% of attempted, or rejections above 0.1%, or the pool reached its cap |
| `validator_exits`, `host_load_1m` | Any validator that exited during the run; the median and max 1-minute host load in the window |
| `peak_memory_bytes_nodes` | Each validator container's largest memory use over the 5 s samples (Docker stats: usage minus inactive file) |
| `log_counts` | Per validator, from its log followed for the whole run: `unknown parent vertex` errors, selected-owner Work refusals by `operation` (retried or not) and Work-admission limit errors |

The generator delivers 3,000 TPS on M2 loopback at about 1.1 cores, with requests even
across all ports. It runs on the benchmark host, and its CPU is part of the recorded host
load.

## Old harness

The `.work-in-progress/performance-20261113` speed-measurement scripts are deprecated, to be
deleted once parity is confirmed. Its functional fleet scripts stay until this repository or
node e2e covers them; the list is in that directory's `DEPRECATED.md`.
