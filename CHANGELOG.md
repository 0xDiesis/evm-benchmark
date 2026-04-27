# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-04-27

### Added

- New bench targets: Arbitrum (nitro-testnode, port 8547, chain ID 412346),
  Optimism (Supersim, port 9545, chain ID 901), and Scroll (standalone l2geth
  with zkEVM config, port 48545, chain ID 53077). All three are registered in
  the `lib.sh` chain registry.
- Standalone Foundry project under `contracts/` containing the five benchmark
  fixtures (`BenchmarkMixer`, `BenchmarkToken`, `BenchmarkNFT`, `BenchmarkPair`,
  `BenchmarkRegistry`). Build with `cd contracts && forge build`; the existing
  46-test suite passes across 5 specs.
- `make bytecode` target that regenerates
  `crates/evm-benchmark/bytecode/{Token,Pair,NFT}.hex` from `contracts/` via
  `forge build` + `jq`, keeping embedded bytecode in sync with sources.
- `make bytecode-check` target that fails CI if committed `.hex` files drift
  from what the current sources produce.
- Coverage report publishing in CI, gated behind a repository variable.
- Foundry artifact caching in CI.
- New tests/cli_main.rs integration tests and broader unit-test coverage across
  `cache`, `funding`, `submission/*`, `modes/*`, `reporting/json`, `setup`, and
  `validators/health_monitor`.
- `crates/evm-benchmark/src/submission/rpc_dispatcher.rs`.

### Changed

- Hardened benchmark runtime and RPC flows: substantial refactors across
  `main.rs`, `modes/{burst,ceiling,sustained}.rs`, `submission/*`, `funding.rs`,
  `setup.rs`, and `cache.rs`.
- Regenerated benchmark bytecode from `contracts/src/benchmark/`. Both old and
  new builds use solc 0.8.34 with optimizer 200 and `via_ir = true`. Behavior
  is unchanged (harness exercises success paths only); bytecode shrank as a
  result of replacing `require`-string reverts with custom errors:
  `BenchmarkToken` 4228 → 4010 chars (-5%), `BenchmarkPair` 4596 → 3822 chars
  (-17%), `BenchmarkNFT` 4638 → 4462 chars (-4%).
- Diesis chain benchmark profile tuned; FEC geo sweep results recorded under
  `docs/`.
- Graceful fallback when the private Diesis source repo is missing: the
  `bench-targets/` scripts and `chains/diesis/Makefile` now emit actionable
  errors instead of obscure `cd`/`make` failures, and
  `chains/diesis/README.md` no longer points into a maintainer-only path.
- Replaced en-dashes (U+2013) with hyphens (U+002D) across README, docs, and
  the network-topology shell script. Mechanical, no behavior change.
- Help banner reframed as chain-agnostic ("EVM Benchmark Suite") with Diesis
  listed as one of N supported chains.
- Pinned Rust toolchain to 1.95 via `rust-toolchain.toml` and pinned
  `dtolnay/rust-toolchain@1.95.0` across all three workflows so local and CI
  stay locked together when stable advances.

### Removed

- Unused `diesis-*` Makefile convenience targets (`diesis-up/down/restart/
  status/quick/full/tune/geo`). They had zero references and the cluster-mgmt
  variants required the private Diesis repo to be a sibling. The generic
  `bench`/`compare`/`sweep` targets cover the same ground (e.g.
  `make bench CHAIN=diesis ...`).

### Fixed

- doublcov coverage workflow usage.
- Clippy warnings introduced across Rust toolchain bumps:
  - 1.94: `await_holding_lock`, `useless_conversion`, `let_and_return`,
    `if_same_then_else`, `useless_vec`.
  - 1.95: `manual_checked_ops`, `unnecessary_min_or_max`.
- Coverage publishing now reads the page-publishing toggle from a repository
  variable.

## [0.1.0] - Initial release

[0.2.0]: https://github.com/0xDiesis/evm-benchmark/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/0xDiesis/evm-benchmark/releases/tag/v0.1.0
