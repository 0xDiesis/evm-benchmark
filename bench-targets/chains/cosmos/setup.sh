#!/bin/bash
# setup.sh — Verify dependencies for the Cosmos/Evmos benchmark target.
# The evmosd binary is built inside Docker; no host Go installation needed.
set -euo pipefail

require_cmd() {
    local cmd="$1"
    local hint="$2"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "ERROR: '$cmd' is required. ${hint}" >&2
        exit 1
    fi
}

require_cmd docker "Install Docker Desktop: https://www.docker.com/products/docker-desktop/"
require_cmd "docker" "docker compose plugin required (Docker Desktop includes it)"

# Verify compose plugin
if ! docker compose version >/dev/null 2>&1; then
    echo "ERROR: 'docker compose' plugin not found. Install Docker Desktop." >&2
    exit 1
fi

echo "Dependencies OK. Run 'make up' to build and start the Evmos node."
echo "Note: First build downloads Go toolchain + compiles evmosd (~5-10 min)."
