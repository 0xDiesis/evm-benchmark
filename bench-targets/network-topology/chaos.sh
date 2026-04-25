#!/bin/bash
set -euo pipefail

# chaos.sh — Start/stop Pumba network chaos for Diesis e2e benchmarks.
#
# Usage:
#   ./docker/pumba/chaos.sh start [profile]    Start chaos (default: global)
#   ./docker/pumba/chaos.sh stop               Stop and remove Pumba containers
#   ./docker/pumba/chaos.sh status             Show current chaos state
#   ./docker/pumba/chaos.sh profiles           List available profiles
#
# Profiles: continental, global, clustered, degraded
#
# The e2e cluster must be running before starting chaos.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_REPO_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
# DIESIS_REPO_DIR points at the (private) Diesis source repo for compose files and
# cluster lifecycle. Falls back to the conventional sibling layout when unset.
DIESIS_REPO_DIR="${DIESIS_REPO_DIR:-${BENCH_REPO_DIR}/../diesis}"
if [[ ! -d "${DIESIS_REPO_DIR}" ]]; then
    echo "ERROR: this script requires the Diesis source repo at \${DIESIS_REPO_DIR}." >&2
    echo "       Not found at: ${DIESIS_REPO_DIR}" >&2
    echo "       Set DIESIS_REPO_DIR to a Diesis checkout and re-run." >&2
    exit 2
fi
COMPOSE_E2E="${DIESIS_REPO_DIR}/docker/docker-compose.e2e.yml"
COMPOSE_PUMBA="${DIESIS_REPO_DIR}/docker/docker-compose.pumba.yml"
PROFILES_ENV="${SCRIPT_DIR}/profiles.env"

# shellcheck source=profiles.env
source "${PROFILES_ENV}"

compose() {
    docker compose -f "${COMPOSE_E2E}" -f "${COMPOSE_PUMBA}" "$@"
}

check_e2e_running() {
    if ! docker ps --format '{{.Names}}' | grep -q 'diesis-e2e-node-1'; then
        echo "ERROR: Diesis e2e cluster is not running."
        echo "Start it first: make e2e-up-release"
        exit 1
    fi
}

cmd_start() {
    local profile="${1:-global}"
    check_e2e_running

    echo "Starting Pumba network chaos with profile: ${profile}"
    apply_profile "${profile}"

    # Stop any existing Pumba containers first
    compose stop pumba-node-1 pumba-node-2 pumba-node-3 pumba-node-4 2>/dev/null || true
    compose rm -f pumba-node-1 pumba-node-2 pumba-node-3 pumba-node-4 2>/dev/null || true

    # Start Pumba sidecars
    local extra_profiles=()
    if [ "${PUMBA_NODE4_LOSS:-0}" -gt 0 ] 2>/dev/null; then
        extra_profiles=(--profile lossy)
    fi
    compose up -d "${extra_profiles[@]}" pumba-node-1 pumba-node-2 pumba-node-3 pumba-node-4

    echo ""
    echo "Network chaos active. Validators are now experiencing simulated latency."
    echo "Run benchmarks with: make e2e-run-bench"
    echo "Stop chaos with: ./docker/pumba/chaos.sh stop"
}

cmd_stop() {
    echo "Stopping Pumba network chaos..."
    compose stop pumba-node-1 pumba-node-2 pumba-node-3 pumba-node-4 pumba-node-4-loss 2>/dev/null || true
    compose rm -f pumba-node-1 pumba-node-2 pumba-node-3 pumba-node-4 pumba-node-4-loss 2>/dev/null || true

    # Clean up any leftover tc helper containers
    docker ps -a --filter "ancestor=gaiadocker/iproute2" -q | xargs -r docker rm -f 2>/dev/null || true

    echo "Network chaos stopped. Validators are back to normal network conditions."
}

cmd_status() {
    echo "Pumba chaos containers:"
    docker ps --filter "name=diesis-pumba" --format "table {{.Names}}\t{{.Status}}\t{{.Command}}" 2>/dev/null || echo "  (none running)"
    echo ""
    echo "TC helper containers:"
    docker ps --filter "ancestor=gaiadocker/iproute2" --format "table {{.Names}}\t{{.Status}}" 2>/dev/null || echo "  (none running)"
}

cmd_profiles() {
    echo "Available network chaos profiles:"
    echo ""
    echo "  continental  — Validators across one continent (5-60ms)"
    echo "                 Realistic for regional validator sets"
    echo ""
    echo "  global       — Validators worldwide (5-200ms)"
    echo "                 US-East, US-West, EU-West, Asia-Pacific"
    echo ""
    echo "  clustered    — 3 co-located + 1 outlier (1-120ms)"
    echo "                 Tests protocol with one slow validator"
    echo ""
    echo "  degraded     — All links congested with high jitter (30-80ms ±20-40ms)"
    echo "                 Stress test for congested networks"
}

case "${1:-help}" in
    start)    cmd_start "${2:-global}" ;;
    stop)     cmd_stop ;;
    status)   cmd_status ;;
    profiles) cmd_profiles ;;
    *)
        echo "Usage: $0 {start [profile]|stop|status|profiles}"
        echo ""
        cmd_profiles
        exit 1
        ;;
esac
