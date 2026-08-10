# EVM Benchmark — Agent Instructions

## Rust Builds

Use `sccache` for Cargo compilation commands to reduce rebuild time: prefix `cargo build`, `cargo check`, `cargo test`, and `cargo clippy` with `RUSTC_WRAPPER=sccache`. Do not run `cargo clean` unless invalid artifacts require it.

For Rust compilation in Docker, follow the workspace BuildKit pattern: cache `/usr/local/cargo/registry`, `/usr/local/cargo/git`, and `/var/cache/sccache`; set `SCCACHE_DIR=/var/cache/sccache`, `SCCACHE_CACHE_SIZE=20G`, and `RUSTC_WRAPPER=sccache`. Avoid `--no-cache` unless diagnosing a proven cache problem.

## Scratch / Temporary Files

Put ad-hoc logs, debug output, and scratch files in `tmp/`. This directory is
gitignored. Do not commit log files to the repo root.
