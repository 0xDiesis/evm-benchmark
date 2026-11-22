# Diesis benchmark methodology

All Diesis load generation and speed measurement runs through this repository. This page
covers:
- the official procedure;
- what the Diesis target adds to the generic harness;
- every reported metric.

## The official benchmark

EVM targets are judged under the **`capped`** profile:
- 4 validators, each with 2 pinned CPUs and 4 GiB;
- 600 s measured windows (after 60 s of warmup);
- a chain aged past 5,000 blocks;
- interleaved A B B A arms, at least 3 per side;
- medians and ranges reported.

**Primary results:**
- finalized TPS;
- **submit→finalized** p50/p95/p99, against the target of ≤1.5 s;
- **submit→receipt** p50/p95/p99.

**Secondary:** latest→finalized, then the per-block and resource attribution below.

`uncapped` is for diagnosis only.

**Known failure: the official capped run does not complete today.** A capped validator
cannot start an aged chain within its 1 GiB selected encoded Work budget. After about 280
blocks aged under load, node 6570bb686 refused at startup with
`certified history Work admission: EncodedWorkBytes requires 1084835969, configured limit is
1073741824`, and the other validators then lost quorum (`MPT proposal quorum deadline`). This
is a node defect, owned by the V2 Work-admission workstream: Work charges grow with retained
history. The procedure stays as written and is expected to fail until the node fix lands.
Fresh capped fleets and uncapped aged fleets run.

```bash
# Image for the node commit under test (heavy: run through the compute broker).
# The checkout must be clean and contracts/out a real directory (see build-image.sh).
compute run --class heavy --where local -- bash bench-targets/chains/diesis/build-image.sh <node-checkout>

# Aged fleet, once per image storage format (about 45 minutes):
make diesis-age TAG=m6570-5k DIESIS_REPO_DIR=<node-checkout>

# A/B of a change against main at the judged profile:
make ab NAME=<change> AGED=m6570-5k RATES=2000 \
  A="profile=capped,image=<main-sha>-release,repo=<main-checkout>" \
  B="profile=capped,image=<candidate-sha>-release,repo=<candidate-checkout>,name=<change>"

# Baseline under both profiles:
make ab NAME=baseline-<sha> AGED=m6570-5k RATES=500,1000,2000,3000 A="profile=capped" B="profile=uncapped"

# One ad-hoc run (fresh genesis unless AGED is set):
make bench CHAIN=diesis MODE=sustained TPS=2000 DURATION=660 WARMUP=60 PROFILE=capped
```

On the shared M2, do not reserve the host: there is no `compute measure begin --host local`,
and development comes first. Before each arm, `ab.py` waits up to 10 minutes for 60 s of
quiet (1-minute load below 8, no compiler or docker build) and records the outcome in
`arm-*/host-*.json`. The observer records the host load for every window. Treat arms run on
a busy host as diagnostic. Set `BENCH_RESULTS_BASE` to keep results off the system disk
(in this workspace, `/Volumes/Scratch/0xdiesis/reports/evm-benchmark`).

## What the harness already provided

| Area | Existing capability |
|---|---|
| Execution modes | burst (waves), sustained (pre-signed, paced, `target hit` at 95%), ceiling (adaptive step search, saturation confidence score, restart between steps) |
| Environments | `ENV=clean` and eight `geo-*` tc-netem layouts with topology verification and geo-safe consensus timing |
| Ingress | Diesis RPC limiter kept on but raised (`BENCH_DIESIS_RATE_LIMIT_*`), HTTP or WebSocket submission, multi-endpoint round robin with failover, retry profiles |
| Accounting | attempted / accepted / failed / confirmed / pending, `valid` and `invalid_reason`, confirmed %, per-wave latency, finality-confirmation depth mode, cost metrics, preflight |
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
- **Ownership.** Container names stay `diesis-e2e-node-N`, so `network-topology.sh` works.
  The launcher refuses to start if those names belong to another compose project (for
  example a `make e2e-up` cluster) or if the ports are in use. `down` removes only its own
  fresh project (`diesis-bench`).
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
| `finalized_tps` | Transactions in blocks whose all-four `finalized` head passed them inside the window, ÷ window |
| `submit_to_finalized_s` p50/p95/p99 | Every transaction submitted inside the window: time from submission until all four validators' finalized head reached its block. Transactions never included, or not finalized by the end of the drain (`BENCH_DIESIS_DRAIN_SECS`, default 150), count as infinite, so a percentile that lands on them is `null`; `unfinalized_share` gives their share |
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

The generator delivers 3,000 TPS on M2 loopback at about 1.1 cores, with requests even
across all ports. It runs on the benchmark host, and its CPU is part of the recorded host
load.

## Old harness

The `.work-in-progress/performance-20261113` speed-measurement scripts are deprecated, to be
deleted once parity is confirmed. Its functional fleet scripts stay until this repository or
node e2e covers them; the list is in that directory's `DEPRECATED.md`.
