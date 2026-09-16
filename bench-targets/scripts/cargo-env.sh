#!/bin/bash
# Shared Cargo setup for local benchmark launchers.

# Use sccache by default while preserving explicit caller overrides, including
# RUSTC_WRAPPER= to disable the wrapper for one command.
if [[ -z "${RUSTC_WRAPPER+x}" ]]; then
    export RUSTC_WRAPPER=sccache
fi

cargo_target_directory() {
    local manifest_path="$1"
    cargo metadata --no-deps --format-version 1 --manifest-path "${manifest_path}" |
        python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])'
}
