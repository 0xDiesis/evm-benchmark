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
| `DIESIS_*`, `RAYON_*`, `E2E_*`, `BENCH_DIESIS_ENV` | Validator overrides on top of the profile |
| `BENCH_WARMUP_SECS` / `WARMUP=` | Seconds of load excluded from the measured window |

## Endpoints

- Chain ID `19803` (e2e genesis; discovered through `eth_chainId`).
- RPC `127.0.0.1:18545, 18555, 18565, 18575`; WS `127.0.0.1:18546`.
- Metrics `127.0.0.1:19101-19104`; Prometheus `127.0.0.1:19090`.
- Benchmark keys: the deterministic validator keys `1-4`. `--fund` provisions more senders.
