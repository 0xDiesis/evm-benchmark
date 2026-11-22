# Diesis — Benchmark Target

The Diesis target runs the harness against a four-validator fleet started by `fleet.py`. The
fleet comes from an existing `diesis-node-e2e` image and the node checkout that matches it.
Methodology, the official run rule and metric definitions:
[docs/diesis-methodology.md](../../../docs/diesis-methodology.md).

## Quick start

```bash
# Image for the checkout's HEAD (heavy; run through the compute broker on shared hosts)
make image DIESIS_REPO_DIR=<node-checkout>
# One sustained run under the official profile, from a fresh genesis
make -C ../../.. bench CHAIN=diesis MODE=sustained TPS=1000 DURATION=60
make status
make down
```

## Existing fleet

To benchmark a fleet that is already running (started elsewhere), use `FLEET=external`
(`DIESIS_FLEET=external`) with its endpoints. The orchestrator then never starts, stops,
rebuilds or restarts it, and never reads `DIESIS_REPO_DIR`:

```bash
make bench CHAIN=diesis FLEET=external \
  DIESIS_RPC=http://127.0.0.1:38545,http://127.0.0.1:38555,http://127.0.0.1:38565,http://127.0.0.1:38575 \
  DIESIS_WS=ws://127.0.0.1:38546 MODE=sustained TPS=1000 DURATION=60
```

## Inputs

| Variable | Meaning |
|---|---|
| `DIESIS_FLEET` / `FLEET=` | `external` targets a running fleet at `DIESIS_RPC` / `DIESIS_WS`; unset runs `fleet.py` |
| `DIESIS_CHAIN_ID` / `CHAIN_ID=` | Chain ID; default the first endpoint's `eth_chainId` |
| `DIESIS_REPO_DIR` | Node checkout rendered for the fleet (default `../diesis` beside this repository) |
| `DIESIS_IMAGE_TAG` / `IMAGE=` | `diesis-node-e2e` tag (default `<checkout HEAD>-release`); must exist |
| `BENCH_DIESIS_PROFILE` / `PROFILE=` | `capped` (default, the run rule) or `uncapped` (diagnosis), from `profiles/` |
| `DIESIS_AGED_TAG` / `AGED=` | Reuse a stopped aged fleet (`make diesis-age TAG=…`) |
| `BENCH_DIESIS_FLEET` | This user's fresh-fleet name (default `diesis-bench`) |
| `BENCH_DIESIS_OUTCOME_RULE` | Genesis outcome signature rule: `bls-only` (default) or `hybrid`. Hybrid fleets stop within a few blocks until the node's live validator path signs ML-DSA halves. Checkouts without `docker/epoch/epoch-zero-4v-bls-only.calldata` predate the control and get no rule keys |
| `BENCH_DIESIS_ENV`, `E2E_*` | Validator overrides on top of the profile (ambient `DIESIS_*` only feed compose interpolation) |
| `BENCH_WARMUP_SECS` / `WARMUP=` | Seconds of load excluded from the measured window |

## Endpoints

- Chain ID `19803` (e2e genesis; discovered through `eth_chainId`).
- Every fleet takes the first free port block. Block 0 is RPC `127.0.0.1:18545, 18555, 18565,
  18575`, WS `18546`, metrics `19101-19104` and Prometheus `19090`; block k adds 200·k. The
  chosen block is in `$BENCH_RESULTS_BASE/fleet/<project>/ports.json`, and
  `python3 fleet.py ports` prints it.
- Set `BENCH_DIESIS_FLEET=<name>` when another user runs a fleet on the same host.
- Benchmark keys: the deterministic validator keys `1-4`. `--fund` provisions more senders.
