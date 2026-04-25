# Diesis — Benchmark Target

This target adapter runs the chain-agnostic benchmark harness against a Diesis network.

Operational playbooks for the Diesis chain (cluster bring-up, sweeps, geo profiles) require the Diesis source repository. If you have access, set `DIESIS_REPO_DIR` to its path; otherwise, scripts in this directory will exit with a clear error.

## Assumptions

- If you have a checkout of the Diesis source repository, place it at `../diesis/` (or set `DIESIS_REPO_DIR` to its path). Without it, only the chain-targeting scripts that talk to an already-running cluster will work.
- Diesis e2e validators expose RPC on `8545, 8555, 8565, 8575`
- The default benchmark keys are the deterministic validator keys from Diesis genesis

## Quick Start

```bash
make up
make bench
make status
make down
```

## Defaults

- Chain ID: `19803`
- RPC endpoints: `http://localhost:8545,http://localhost:8555,http://localhost:8565,http://localhost:8575`
- WS endpoint: `ws://localhost:8546`
- Benchmark keys: deterministic validator keys `1-4`

Override `DIESIS_REPO_DIR`, `DIESIS_RPC`, `DIESIS_WS`, `DIESIS_CHAIN_ID`, `BENCH_KEY`, `BENCH_NAME`, or `BENCH_OUT` as needed.

This README intentionally stays adapter-focused. Diesis comparison methodology and benchmarking recommendations are maintained in the Diesis repo docs.
