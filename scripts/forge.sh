#!/bin/sh
# Prefer the local Foundry fork; standard Foundry remains usable on other hosts.
# FORGE_BIN selects one executable, never a shell command or argument string.
set -eu
# Diesis ABI and bytecode consumers require these paths; forge-ds uses different defaults.
export FOUNDRY_OUT="${FOUNDRY_OUT:-out}"
export FOUNDRY_CACHE_PATH="${FOUNDRY_CACHE_PATH:-cache}"
if [ -n "${FORGE_BIN:-}" ]; then
    selected=$(command -v "$FORGE_BIN") || {
        echo "FORGE_BIN executable not found: $FORGE_BIN" >&2
        exit 127
    }
elif selected=$(command -v forge-ds); then
    :
elif selected=$(command -v forge); then
    :
else
    echo "Install forge-ds or forge, or set FORGE_BIN to an executable." >&2
    exit 127
fi
# Keep relative executable overrides valid if a nested tool changes directory.
case "$selected" in
    /*) ;;
    *) selected="$PWD/$selected" ;;
esac
if [ "${1:-}" = "--exec" ]; then
    shift
    if [ "$#" -eq 0 ]; then
        echo "Usage: forge.sh --exec COMMAND [ARGS...]" >&2
        exit 2
    fi
    script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
    export DIESIS_FORGE_EXECUTABLE="$selected" FORGE_BIN="$selected"
    export PATH="$script_dir/foundry-bin:$PATH"
    exec "$@"
fi
exec "$selected" "$@"
