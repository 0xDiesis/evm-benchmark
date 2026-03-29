#!/bin/bash
set -euo pipefail

# apply-chaos.sh — Chain-agnostic Pumba network chaos for any Docker-based
#                  validator cluster. Works with Diesis, Sonic, Avalanche, etc.
#
# Usage:
#   ./apply-chaos.sh start <container1> <container2> ... [--profile <profile>]
#   ./apply-chaos.sh stop  <container1> <container2> ...
#   ./apply-chaos.sh stop-all
#   ./apply-chaos.sh profiles
#
# Examples:
#   # Apply "global" latency profile to Diesis e2e nodes:
#   ./apply-chaos.sh start diesis-e2e-node-1 diesis-e2e-node-2 diesis-e2e-node-3 diesis-e2e-node-4 --profile global
#
#   # Apply to Sonic nodes:
#   ./apply-chaos.sh start sonic-node-1 sonic-node-2 sonic-node-3 sonic-node-4 --profile continental
#
#   # Stop chaos on specific containers:
#   ./apply-chaos.sh stop diesis-e2e-node-1 diesis-e2e-node-2 diesis-e2e-node-3 diesis-e2e-node-4
#
# The script assigns increasing latency to each container in order:
#   - Container 1: Region A (closest)
#   - Container 2: Region A (nearby)
#   - Container 3: Region B (medium distance)
#   - Container 4: Region C (farthest)
# Additional containers cycle through regions B and C.

PUMBA_IMAGE="${PUMBA_IMAGE:-ghcr.io/alexei-led/pumba:latest}"
TC_IMAGE="${TC_IMAGE:-gaiadocker/iproute2}"
CHAOS_DURATION="${CHAOS_DURATION:-24h}"

# ---- Profile definitions ----
# Format: delay_ms,jitter_ms for each region tier
# Tiers: near, nearby, medium, far

declare -A PROFILES
PROFILES[continental]="5,2|20,5|40,8|60,12"
PROFILES[global]="5,2|30,8|80,15|200,40"
PROFILES[clustered]="1,1|2,1|3,1|120,25"
PROFILES[degraded]="30,20|40,25|50,30|80,40"

parse_profile() {
    local profile="${1}"
    local tier="${2}"  # 0-3

    if [[ -z "${PROFILES[$profile]+x}" ]]; then
        echo "ERROR: Unknown profile '${profile}'. Available: ${!PROFILES[*]}" >&2
        exit 1
    fi

    local tiers
    IFS='|' read -ra tiers <<< "${PROFILES[$profile]}"
    # Cycle through tiers if more containers than tiers
    local idx=$(( tier % ${#tiers[@]} ))
    # For tier >= 2, always use tier 2+ (medium/far)
    if [[ $tier -ge ${#tiers[@]} ]]; then
        idx=$(( 2 + (tier % (${#tiers[@]} - 2)) ))
    fi
    echo "${tiers[$idx]}"
}

cmd_start() {
    local profile="global"
    local containers=()

    # Parse args: containers first, then --profile <name>
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --profile|-p) profile="$2"; shift 2 ;;
            --*)          echo "Unknown option: $1" >&2; exit 1 ;;
            *)            containers+=("$1"); shift ;;
        esac
    done

    if [[ ${#containers[@]} -eq 0 ]]; then
        echo "ERROR: No containers specified." >&2
        echo "Usage: $0 start <container1> [container2 ...] [--profile <profile>]" >&2
        exit 1
    fi

    echo "Applying Pumba network chaos"
    echo "  Profile:    ${profile}"
    echo "  Containers: ${containers[*]}"
    echo ""

    for i in "${!containers[@]}"; do
        local container="${containers[$i]}"
        local tier_data
        tier_data=$(parse_profile "${profile}" "$i")
        local delay jitter
        IFS=',' read -r delay jitter <<< "${tier_data}"

        local pumba_name="pumba-chaos-${container}"

        # Stop existing chaos for this container
        docker rm -f "${pumba_name}" 2>/dev/null || true

        echo "  ${container}: ${delay}ms ± ${jitter}ms (normal distribution)"

        docker run -d \
            --name "${pumba_name}" \
            --rm \
            -v /var/run/docker.sock:/var/run/docker.sock \
            "${PUMBA_IMAGE}" \
            --log-level info \
            netem \
            --duration "${CHAOS_DURATION}" \
            --tc-image "${TC_IMAGE}" \
            delay \
            --time "${delay}" \
            --jitter "${jitter}" \
            --distribution normal \
            "${container}" >/dev/null
    done

    echo ""
    echo "Network chaos active."
}

cmd_stop() {
    local containers=("$@")

    if [[ ${#containers[@]} -eq 0 ]]; then
        echo "ERROR: No containers specified. Use 'stop-all' to stop all chaos." >&2
        exit 1
    fi

    for container in "${containers[@]}"; do
        local pumba_name="pumba-chaos-${container}"
        docker rm -f "${pumba_name}" 2>/dev/null && \
            echo "Stopped chaos for ${container}" || true
    done

    # Clean up tc helper containers
    docker ps -a --filter "ancestor=${TC_IMAGE}" -q | xargs -r docker rm -f 2>/dev/null || true
    echo "Done."
}

cmd_stop_all() {
    echo "Stopping all Pumba chaos containers..."
    docker ps -a --filter "name=pumba-chaos-" -q | xargs -r docker rm -f 2>/dev/null || true
    docker ps -a --filter "ancestor=${TC_IMAGE}" -q | xargs -r docker rm -f 2>/dev/null || true
    echo "All network chaos stopped."
}

cmd_profiles() {
    echo "Available network chaos profiles:"
    echo ""
    for name in "${!PROFILES[@]}"; do
        local tiers
        IFS='|' read -ra tiers <<< "${PROFILES[$name]}"
        echo "  ${name}:"
        local labels=("near" "nearby" "medium" "far")
        for i in "${!tiers[@]}"; do
            local delay jitter
            IFS=',' read -r delay jitter <<< "${tiers[$i]}"
            echo "    ${labels[$i]:-extra}: ${delay}ms ± ${jitter}ms"
        done
        echo ""
    done
}

case "${1:-help}" in
    start)     shift; cmd_start "$@" ;;
    stop)      shift; cmd_stop "$@" ;;
    stop-all)  cmd_stop_all ;;
    profiles)  cmd_profiles ;;
    *)
        echo "Usage: $0 {start|stop|stop-all|profiles}"
        echo ""
        echo "  start <container...> [--profile <name>]  Apply network chaos"
        echo "  stop <container...>                       Stop chaos on containers"
        echo "  stop-all                                  Stop all Pumba chaos"
        echo "  profiles                                  List profiles"
        exit 1
        ;;
esac
