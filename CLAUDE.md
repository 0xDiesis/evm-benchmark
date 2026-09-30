# EVM Benchmark — Agent Instructions

## Rust Builds

Compiler caching is optional. Use `DIESIS_USE_SCCACHE=1 make build` or explicitly prefix Cargo commands with `RUSTC_WRAPPER=sccache` to enable it. Default builds do not require sccache or Scratch configuration. Do not run `cargo clean` unless invalid artifacts require it.

For Rust compilation in Docker, follow the workspace BuildKit pattern: cache `/usr/local/cargo/registry`, `/usr/local/cargo/git`, and `/var/cache/sccache`; set `SCCACHE_DIR=/var/cache/sccache`, `SCCACHE_CACHE_SIZE=20G`, and `RUSTC_WRAPPER=sccache`. Avoid `--no-cache` unless diagnosing a proven cache problem.

## Scratch / Temporary Files

Put ad-hoc logs, debug output, and scratch files in `tmp/`. This directory is
gitignored. Do not commit log files to the repo root.
