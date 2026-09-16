#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_REPO_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HELPER="${SCRIPT_DIR}/cargo-env.sh"

# The single-quoted snippets must expand in their child bash processes.
# shellcheck disable=SC2016
env -u RUSTC_WRAPPER bash -c \
    'source "$1"; [[ "$RUSTC_WRAPPER" == sccache ]]' _ "${HELPER}"
# shellcheck disable=SC2016
RUSTC_WRAPPER=custom-wrapper bash -c \
    'source "$1"; [[ "$RUSTC_WRAPPER" == custom-wrapper ]]' _ "${HELPER}"
# shellcheck disable=SC2016
RUSTC_WRAPPER='' bash -c \
    'source "$1"; [[ -n "${RUSTC_WRAPPER+x}" && -z "$RUSTC_WRAPPER" ]]' _ "${HELPER}"

expected="${BENCH_REPO_DIR}/tmp/cargo-env-test-target"
actual="$({ CARGO_TARGET_DIR="${expected}" bash -c \
    'source "$1"; cargo_target_directory "$2"' _ \
    "${HELPER}" "${BENCH_REPO_DIR}/Cargo.toml"; })"
[[ "${actual}" == "${expected}" ]]

echo "cargo-env: defaults, overrides, and target routing passed"
