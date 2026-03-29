# Diesis — Benchmark Target

This target adapter runs the chain-agnostic benchmark harness against a Diesis network.

For Diesis-specific performance tuning, sweeps, and operational playbooks, see `../../../../diesis/docs/benchmarking.md` in the Diesis repository.

## Assumptions

- `evm-benchmark/` and `diesis/` live side by side
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
