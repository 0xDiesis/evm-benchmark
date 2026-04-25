# benchmark contracts

Self-contained Foundry project housing the e2e benchmark fixture contracts
consumed by the `evm-benchmark` Rust harness.

These contracts are intentionally minimal and not for production use:

- `BenchmarkMixer` — storage + computation + event mixing workload
- `BenchmarkToken` — minimal ERC-20 with open mint
- `BenchmarkNFT` — minimal ERC-721 with open mint
- `BenchmarkPair` — simplified Uniswap-style constant-product AMM
- `BenchmarkRegistry` — key-value registry

Build:

```
forge build
```

Run tests:

```
forge test
```

The Rust harness in `crates/evm-benchmark/` embeds creation bytecode for
`BenchmarkNFT`, `BenchmarkPair`, and `BenchmarkToken` from this project.
Bytecode files at `crates/evm-benchmark/bytecode/*.hex` should be
regenerated from `out/<Contract>.sol/<Contract>.json` after any contract
change. (Currently regenerated manually; consider a `make build-bytecode`
target if drift becomes an issue.)
