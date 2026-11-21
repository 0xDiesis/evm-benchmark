# Diesis — Benchmark Target

This target adapter runs the chain-agnostic benchmark harness against a Diesis network.

Operational playbooks for the Diesis chain (cluster bring-up, sweeps, geo profiles) require the Diesis source repository. If you have access, set `DIESIS_REPO_DIR` to its path; otherwise, scripts in this directory will exit with a clear error.

## Assumptions

- If you have a checkout of the Diesis source repository, place it at `../diesis/` (or set `DIESIS_REPO_DIR` to its path). Without it, only the chain-targeting scripts that talk to an already-running cluster will work.
- Diesis e2e validators expose RPC on `18545, 18555, 18565, 18575`
- The default benchmark keys are the deterministic validator keys from Diesis genesis

## Quick Start

```bash
make up
make bench
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

## Defaults

- Chain ID: `DIESIS_CHAIN_ID` (`CHAIN_ID=` from make), else the first endpoint's `eth_chainId` (e2e: `19803`)
- RPC endpoints: `http://localhost:18545,http://localhost:18555,http://localhost:18565,http://localhost:18575`
- WS endpoint: `ws://localhost:18546`
- Benchmark keys: deterministic validator keys `1-4`

Override `DIESIS_REPO_DIR`, `DIESIS_RPC`, `DIESIS_WS`, `DIESIS_CHAIN_ID`, `BENCH_KEY`, `BENCH_NAME`, or `BENCH_OUT` as needed.

This README intentionally stays adapter-focused. Diesis comparison methodology and benchmarking recommendations are maintained in the Diesis repo docs.
