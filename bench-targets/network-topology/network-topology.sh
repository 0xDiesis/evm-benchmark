#!/bin/bash
set -euo pipefail

# network-topology.sh — Apply pairwise network latency between validator nodes
#                       using Linux TC (traffic control) with netem and u32 filters.
#
# Unlike flat per-node delays (Pumba), this gives true geographic simulation where
# co-located nodes have low latency between each other while distant nodes have high latency.
#
# Usage:
#   ./network-topology.sh apply <layout>     Apply a geographic layout
#   ./network-topology.sh clear              Remove all tc rules
#   ./network-topology.sh status             Show current tc rules per node
#   ./network-topology.sh verify             Ping between all pairs to measure actual RTT
#   ./network-topology.sh layouts            List available layouts
#
# Layouts define a latency matrix between node pairs (RTT in ms):
#   global-spread:  US-East, US-West, EU-Frankfurt, Asia-Tokyo
#   us-distributed: US-East, US-West, US-Central, US-South
#   eu-cluster:     EU-Frankfurt x3 + US-East outlier
#   se-asia-regional: Singapore, Jakarta, Bangkok, Manila
#   europe-regional:  London, Frankfurt, Dublin, Paris
#   south-america-regional: São Paulo, Santiago, Buenos Aires, Bogotá
#   degraded-wan:   All links congested with high jitter
#
# Requirements:
#   - Docker containers with NET_ADMIN capability
#   - iproute2 installed in the container image
#   - Static IPs assigned in docker-compose (10.100.0.11-14)

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

# Default Diesis e2e node mapping (override with env vars for other chains)
NODE1_CONTAINER="${NODE1_CONTAINER:-diesis-e2e-node-1}"
NODE2_CONTAINER="${NODE2_CONTAINER:-diesis-e2e-node-2}"
NODE3_CONTAINER="${NODE3_CONTAINER:-diesis-e2e-node-3}"
NODE4_CONTAINER="${NODE4_CONTAINER:-diesis-e2e-node-4}"

NODE1_IP="${NODE1_IP:-10.100.0.11}"
NODE2_IP="${NODE2_IP:-10.100.0.12}"
NODE3_IP="${NODE3_IP:-10.100.0.13}"
NODE4_IP="${NODE4_IP:-10.100.0.14}"

CONTAINERS=("$NODE1_CONTAINER" "$NODE2_CONTAINER" "$NODE3_CONTAINER" "$NODE4_CONTAINER")
IPS=("$NODE1_IP" "$NODE2_IP" "$NODE3_IP" "$NODE4_IP")
LABELS=("node-1" "node-2" "node-3" "node-4")

# ============================================================================
# Geographic Layout Definitions
#
# Each layout is a 4x4 RTT matrix (ms). Diagonal is 0 (self).
# Format: LAYOUT_name[i:j] = RTT in ms between node-i and node-j
#
# The script splits each RTT in half and applies one netem qdisc per directed
# link. Both endpoints apply their half, so the measured round trip equals the
# matrix RTT (delay rtt/2 on A->B egress + delay rtt/2 on B->A egress = rtt).
#
# Each layout also defines realistic link quality, not just propagation delay:
#   * LAYOUT_JITTER_PCT / LAYOUT_JITTER_CAP_MS — one-way jitter as a fraction of
#     the one-way delay, capped in absolute terms. Backbone/submarine links are
#     timing-stable (a few ms of jitter regardless of distance), so jitter is
#     decoupled from delay rather than scaling with it; congested layouts leave
#     it uncapped for heavy variance.
#   * LAYOUT_LOSS_PCT / LAYOUT_LOSS_CORR_PCT — per-direction packet loss with a
#     correlation (Gilbert-Elliott-style burstiness). Real long-haul links are
#     never lossless; modelling loss is what actually exercises the protocol's
#     retransmit and FEC shred-repair paths, so loss tuning measured here
#     reflects real-world conditions instead of an idealised lossless fabric.
# (Bandwidth/rate limiting is intentionally not modelled here; enable netem
#  `rate` per link if shred-propagation bandwidth becomes the variable of study.)
# ============================================================================

declare -A LAYOUT

define_layout_global_spread() {
    # Simulates: node-1=US-East, node-2=US-West, node-3=EU-Frankfurt, node-4=Asia-Tokyo
    # Based on real-world submarine cable + terrestrial backbone measurements.
    LAYOUT_NAME="global-spread"
    LAYOUT_DESC="US-East, US-West, EU-Frankfurt, Asia-Tokyo"
    LAYOUT_LOCATIONS=("US-East/Virginia" "US-West/Oregon" "EU/Frankfurt" "Asia/Tokyo")
    # Backbone/submarine links: very stable timing (low absolute jitter) with a
    # small, slightly bursty baseline loss that exercises FEC shred repair.
    LAYOUT_JITTER_PCT=4; LAYOUT_JITTER_CAP_MS=5
    LAYOUT_LOSS_PCT=0.1; LAYOUT_LOSS_CORR_PCT=25
    #           node-1  node-2  node-3  node-4
    LAYOUT[0:0]=0;   LAYOUT[0:1]=60;  LAYOUT[0:2]=90;  LAYOUT[0:3]=180
    LAYOUT[1:0]=60;  LAYOUT[1:1]=0;   LAYOUT[1:2]=140; LAYOUT[1:3]=120
    LAYOUT[2:0]=90;  LAYOUT[2:1]=140; LAYOUT[2:2]=0;   LAYOUT[2:3]=240
    LAYOUT[3:0]=180; LAYOUT[3:1]=120; LAYOUT[3:2]=240; LAYOUT[3:3]=0
}

define_layout_us_distributed() {
    # Simulates: node-1=US-East, node-2=US-West, node-3=US-Central, node-4=US-South
    # Moderate latencies, all within one continent.
    LAYOUT_NAME="us-distributed"
    LAYOUT_DESC="US-East, US-West, US-Central, US-South"
    LAYOUT_LOCATIONS=("US-East/Virginia" "US-West/Oregon" "US-Central/Iowa" "US-South/Texas")
    # Terrestrial US backbone: very clean — minimal jitter and near-zero loss.
    LAYOUT_JITTER_PCT=3; LAYOUT_JITTER_CAP_MS=3
    LAYOUT_LOSS_PCT=0.02; LAYOUT_LOSS_CORR_PCT=25
    LAYOUT[0:0]=0;   LAYOUT[0:1]=60;  LAYOUT[0:2]=30;  LAYOUT[0:3]=35
    LAYOUT[1:0]=60;  LAYOUT[1:1]=0;   LAYOUT[1:2]=40;  LAYOUT[1:3]=45
    LAYOUT[2:0]=30;  LAYOUT[2:1]=40;  LAYOUT[2:2]=0;   LAYOUT[2:3]=20
    LAYOUT[3:0]=35;  LAYOUT[3:1]=45;  LAYOUT[3:2]=20;  LAYOUT[3:3]=0
}

define_layout_eu_cluster() {
    # Simulates: node-1/2/3 co-located in EU-Frankfurt, node-4=US-East outlier.
    # Tests how protocol handles one distant validator.
    LAYOUT_NAME="eu-cluster"
    LAYOUT_DESC="EU-Frankfurt x3 + US-East outlier"
    LAYOUT_LOCATIONS=("EU/Frankfurt-1" "EU/Frankfurt-2" "EU/Frankfurt-3" "US-East/Virginia")
    # Datacenter cluster + one transatlantic outlier: low jitter, low loss.
    LAYOUT_JITTER_PCT=4; LAYOUT_JITTER_CAP_MS=4
    LAYOUT_LOSS_PCT=0.05; LAYOUT_LOSS_CORR_PCT=25
    LAYOUT[0:0]=0;  LAYOUT[0:1]=2;   LAYOUT[0:2]=2;   LAYOUT[0:3]=90
    LAYOUT[1:0]=2;  LAYOUT[1:1]=0;   LAYOUT[1:2]=2;   LAYOUT[1:3]=90
    LAYOUT[2:0]=2;  LAYOUT[2:1]=2;   LAYOUT[2:2]=0;   LAYOUT[2:3]=90
    LAYOUT[3:0]=90; LAYOUT[3:1]=90;  LAYOUT[3:2]=90;  LAYOUT[3:3]=0
}

define_layout_se_asia_regional() {
    # Simulates a Southeast Asia validator set with all nodes in-region.
    # node-1=Singapore, node-2=Jakarta, node-3=Bangkok, node-4=Manila.
    LAYOUT_NAME="se-asia-regional"
    LAYOUT_DESC="Singapore, Jakarta, Bangkok, Manila"
    LAYOUT_LOCATIONS=("Asia/Singapore" "Asia/Jakarta" "Asia/Bangkok" "Asia/Manila")
    # Regional submarine and metro links: moderate jitter, low bursty loss.
    LAYOUT_JITTER_PCT=5; LAYOUT_JITTER_CAP_MS=4
    LAYOUT_LOSS_PCT=0.08; LAYOUT_LOSS_CORR_PCT=25
    LAYOUT[0:0]=0;  LAYOUT[0:1]=25; LAYOUT[0:2]=30; LAYOUT[0:3]=40
    LAYOUT[1:0]=25; LAYOUT[1:1]=0;  LAYOUT[1:2]=45; LAYOUT[1:3]=55
    LAYOUT[2:0]=30; LAYOUT[2:1]=45; LAYOUT[2:2]=0;  LAYOUT[2:3]=65
    LAYOUT[3:0]=40; LAYOUT[3:1]=55; LAYOUT[3:2]=65; LAYOUT[3:3]=0
}

define_layout_europe_regional() {
    # Simulates a Europe-only validator set across common western EU hubs.
    # node-1=London, node-2=Frankfurt, node-3=Dublin, node-4=Paris.
    LAYOUT_NAME="europe-regional"
    LAYOUT_DESC="London, Frankfurt, Dublin, Paris"
    LAYOUT_LOCATIONS=("EU/London" "EU/Frankfurt" "EU/Dublin" "EU/Paris")
    # Dense European backbone: low jitter and near-zero loss.
    LAYOUT_JITTER_PCT=3; LAYOUT_JITTER_CAP_MS=3
    LAYOUT_LOSS_PCT=0.03; LAYOUT_LOSS_CORR_PCT=25
    LAYOUT[0:0]=0;  LAYOUT[0:1]=20; LAYOUT[0:2]=12; LAYOUT[0:3]=10
    LAYOUT[1:0]=20; LAYOUT[1:1]=0;  LAYOUT[1:2]=25; LAYOUT[1:3]=12
    LAYOUT[2:0]=12; LAYOUT[2:1]=25; LAYOUT[2:2]=0;  LAYOUT[2:3]=18
    LAYOUT[3:0]=10; LAYOUT[3:1]=12; LAYOUT[3:2]=18; LAYOUT[3:3]=0
}

define_layout_south_america_regional() {
    # Simulates a South America validator set across major regional hubs.
    # node-1=São Paulo, node-2=Santiago, node-3=Buenos Aires, node-4=Bogotá.
    LAYOUT_NAME="south-america-regional"
    LAYOUT_DESC="São Paulo, Santiago, Buenos Aires, Bogotá"
    LAYOUT_LOCATIONS=("SA/São Paulo" "SA/Santiago" "SA/Buenos Aires" "SA/Bogotá")
    # Regional paths are longer and peering is less uniform than US/EU.
    LAYOUT_JITTER_PCT=6; LAYOUT_JITTER_CAP_MS=6
    LAYOUT_LOSS_PCT=0.12; LAYOUT_LOSS_CORR_PCT=25
    LAYOUT[0:0]=0;   LAYOUT[0:1]=60;  LAYOUT[0:2]=35; LAYOUT[0:3]=100
    LAYOUT[1:0]=60;  LAYOUT[1:1]=0;   LAYOUT[1:2]=45; LAYOUT[1:3]=90
    LAYOUT[2:0]=35;  LAYOUT[2:1]=45;  LAYOUT[2:2]=0;  LAYOUT[2:3]=105
    LAYOUT[3:0]=100; LAYOUT[3:1]=90;  LAYOUT[3:2]=105; LAYOUT[3:3]=0
}

define_layout_degraded_wan() {
    # All links have high baseline latency + heavy jitter. Simulates congested/
    # unreliable network conditions (peak traffic, DDoS mitigation, poor peering).
    LAYOUT_NAME="degraded-wan"
    LAYOUT_DESC="All links congested with high jitter (stress test)"
    LAYOUT_LOCATIONS=("Degraded-A" "Degraded-B" "Degraded-C" "Degraded-D")
    # Congested/unreliable: heavy jitter (uncapped) and meaningful bursty loss so
    # the protocol's retransmit/FEC-repair behaviour is genuinely exercised.
    LAYOUT_JITTER_PCT=30; LAYOUT_JITTER_CAP_MS=0
    LAYOUT_LOSS_PCT=1.5; LAYOUT_LOSS_CORR_PCT=35
    LAYOUT[0:0]=0;    LAYOUT[0:1]=80;  LAYOUT[0:2]=120; LAYOUT[0:3]=200
    LAYOUT[1:0]=80;   LAYOUT[1:1]=0;   LAYOUT[1:2]=100; LAYOUT[1:3]=180
    LAYOUT[2:0]=120;  LAYOUT[2:1]=100; LAYOUT[2:2]=0;   LAYOUT[2:3]=150
    LAYOUT[3:0]=200;  LAYOUT[3:1]=180; LAYOUT[3:2]=150; LAYOUT[3:3]=0
}

define_layout_intercontinental() {
    # Simulates: Validators on 4 different continents. Worst-case geographic distribution.
    # node-1=US-East, node-2=EU-London, node-3=Asia-Singapore, node-4=South America/São Paulo
    LAYOUT_NAME="intercontinental"
    LAYOUT_DESC="US-East, EU-London, Asia-Singapore, SA-São Paulo"
    LAYOUT_LOCATIONS=("US-East/Virginia" "EU/London" "Asia/Singapore" "SA/São Paulo")
    # Longest submarine paths: stable but with a slightly higher bursty loss
    # baseline than the global-spread backbone (more hops, more repeaters).
    LAYOUT_JITTER_PCT=5; LAYOUT_JITTER_CAP_MS=6
    LAYOUT_LOSS_PCT=0.3; LAYOUT_LOSS_CORR_PCT=25
    LAYOUT[0:0]=0;    LAYOUT[0:1]=75;  LAYOUT[0:2]=230; LAYOUT[0:3]=130
    LAYOUT[1:0]=75;   LAYOUT[1:1]=0;   LAYOUT[1:2]=180; LAYOUT[1:3]=190
    LAYOUT[2:0]=230;  LAYOUT[2:1]=180; LAYOUT[2:2]=0;   LAYOUT[2:3]=340
    LAYOUT[3:0]=130;  LAYOUT[3:1]=190; LAYOUT[3:2]=340; LAYOUT[3:3]=0
}

load_layout() {
    local name="${1}"
    LAYOUT=()
    # Reset per-layout link characteristics to conservative defaults so a layout
    # that omits one of them never inherits the previous layout's value.
    LAYOUT_JITTER_PCT=5      # one-way jitter as a percentage of one-way delay
    LAYOUT_JITTER_CAP_MS=5   # absolute cap on one-way jitter (0 = uncapped)
    LAYOUT_LOSS_PCT=0        # per-direction packet loss percent (fractional ok)
    LAYOUT_LOSS_CORR_PCT=0   # loss correlation percent (burstiness; 0 = i.i.d.)
    case "${name}" in
        global-spread)      define_layout_global_spread ;;
        us-distributed)     define_layout_us_distributed ;;
        eu-cluster)         define_layout_eu_cluster ;;
        se-asia-regional)   define_layout_se_asia_regional ;;
        europe-regional)    define_layout_europe_regional ;;
        south-america-regional) define_layout_south_america_regional ;;
        degraded-wan)       define_layout_degraded_wan ;;
        intercontinental)   define_layout_intercontinental ;;
        *)
            echo "ERROR: Unknown layout '${name}'." >&2
            echo "Available layouts: global-spread, us-distributed, eu-cluster, se-asia-regional, europe-regional, south-america-regional, degraded-wan, intercontinental" >&2
            exit 1
            ;;
    esac
}

# ============================================================================
# TC Rule Application
# ============================================================================

tc_exec() {
    local container="$1"
    shift
    local output
    if ! output=$(docker exec "$container" tc "$@" 2>&1); then
        echo "  WARN: tc command failed on ${container}: tc $*" >&2
        echo "  ${output}" >&2
        return 1
    fi
    [[ -n "$output" ]] && echo "$output"
    return 0
}

ip_to_tc_hex() {
    local ip="$1"
    IFS='.' read -r oct1 oct2 oct3 oct4 <<< "$ip"
    printf '%02x%02x%02x%02x' "$oct1" "$oct2" "$oct3" "$oct4"
}

clear_tc_rules() {
    local container="$1"
    # Delete root qdisc if one exists (restores default pfifo_fast).
    # Failure is expected when no rules are configured yet.
    docker exec "$container" tc qdisc del dev eth0 root 2>/dev/null || true
}

apply_tc_rules() {
    local src_idx="$1"
    local container="${CONTAINERS[$src_idx]}"

    # Collect peers (all nodes except self)
    local peer_indices=()
    for j in "${!CONTAINERS[@]}"; do
        [[ "$j" -eq "$src_idx" ]] && continue
        peer_indices+=("$j")
    done

    local num_peers=${#peer_indices[@]}
    local bands=$((num_peers + 1))

    # Clear existing rules
    clear_tc_rules "$container"

    # Root prio qdisc: band 1 = default (no delay), bands 2+ = peers with delay
    local priomap
    priomap=$(printf '0 %.0s' {1..16})
    tc_exec "$container" qdisc add dev eth0 root handle 1: prio bands "$bands" priomap $priomap

    # Per-layout link quality (with conservative fallbacks).
    local jitter_pct="${LAYOUT_JITTER_PCT:-5}"
    local jitter_cap="${LAYOUT_JITTER_CAP_MS:-5}"
    local loss_pct="${LAYOUT_LOSS_PCT:-0}"
    local loss_corr="${LAYOUT_LOSS_CORR_PCT:-0}"

    local band=2
    for dst_idx in "${peer_indices[@]}"; do
        local rtt_ms="${LAYOUT[$src_idx:$dst_idx]}"
        local one_way_ms=$(( rtt_ms / 2 ))

        # One-way jitter scales with the one-way delay but is capped so stable
        # backbone links don't get unrealistically large absolute jitter.
        local jitter_ms=$(( one_way_ms * jitter_pct / 100 ))
        [[ "$jitter_cap" -gt 0 && "$jitter_ms" -gt "$jitter_cap" ]] && jitter_ms="$jitter_cap"
        [[ "$jitter_ms" -lt 1 ]] && jitter_ms=1

        local dst_ip="${IPS[$dst_idx]}"

        # Build the netem rule: Gaussian-distributed delay, plus correlated
        # (bursty) per-direction packet loss when the layout defines any.
        local -a netem_args=(delay "${one_way_ms}ms" "${jitter_ms}ms" distribution normal)
        if awk "BEGIN{exit !(${loss_pct} > 0)}"; then
            if [[ "$loss_corr" -gt 0 ]]; then
                netem_args+=(loss "${loss_pct}%" "${loss_corr}%")
            else
                netem_args+=(loss "${loss_pct}%")
            fi
        fi

        # Add netem qdisc on this band
        tc_exec "$container" qdisc add dev eth0 parent "1:${band}" handle "${band}0:" \
            netem "${netem_args[@]}"

        # Filter: match destination IP → this band
        tc_exec "$container" filter add dev eth0 parent 1:0 protocol ip u32 \
            match ip dst "${dst_ip}/32" flowid "1:${band}"

        band=$((band + 1))
    done
}

# ============================================================================
# Commands
# ============================================================================

cmd_apply() {
    local layout="${1:-global-spread}"
    load_layout "$layout"

    echo "Applying network topology: ${LAYOUT_NAME}"
    echo "  ${LAYOUT_DESC}"
    echo ""

    # Print latency matrix
    printf "  %-22s" ""
    for j in "${!LABELS[@]}"; do
        printf "%-14s" "${LABELS[$j]}"
    done
    echo ""

    for i in "${!CONTAINERS[@]}"; do
        printf "  %-22s" "${LAYOUT_LOCATIONS[$i]}"
        for j in "${!CONTAINERS[@]}"; do
            if [[ "$i" -eq "$j" ]]; then
                printf "%-14s" "—"
            else
                printf "%-14s" "${LAYOUT[$i:$j]}ms"
            fi
        done
        echo ""
    done
    echo ""
    printf "  Link quality: jitter %s%% of one-way (cap %sms), loss %s%%/dir" \
        "${LAYOUT_JITTER_PCT}" "${LAYOUT_JITTER_CAP_MS}" "${LAYOUT_LOSS_PCT}"
    [[ "${LAYOUT_LOSS_CORR_PCT}" -gt 0 ]] && printf " (%s%% correlated/bursty)" "${LAYOUT_LOSS_CORR_PCT}"
    echo ""
    echo ""

    # Check containers are running
    for container in "${CONTAINERS[@]}"; do
        if ! docker ps --format '{{.Names}}' | grep -q "^${container}$"; then
            echo "ERROR: Container ${container} is not running." >&2
            exit 1
        fi
    done

    # Apply rules to each node
    for i in "${!CONTAINERS[@]}"; do
        apply_tc_rules "$i"
        echo "  Configured ${CONTAINERS[$i]} (${LAYOUT_LOCATIONS[$i]})"
    done

    echo ""
    echo "Network topology active. Run 'verify' to check actual RTTs."
}

cmd_clear() {
    echo "Clearing network topology rules..."
    for container in "${CONTAINERS[@]}"; do
        clear_tc_rules "$container"
        echo "  Cleared ${container}"
    done
    echo "Done — all nodes back to zero-latency networking."
}

cmd_status() {
    for i in "${!CONTAINERS[@]}"; do
        local container="${CONTAINERS[$i]}"
        echo "=== ${container} (${LABELS[$i]}) ==="
        echo "Qdiscs:"
        tc_exec "$container" -s qdisc show dev eth0 || echo "  (no rules)"
        echo ""
        echo "Filters:"
        tc_exec "$container" filter show dev eth0 || echo "  (no filters)"
        echo ""
    done
}

cmd_verify() {
    local layout="${1:-}"
    if [[ -n "$layout" ]]; then
        load_layout "$layout"
        echo "Verifying actual RTTs for layout: ${LAYOUT_NAME}"
    else
        echo "Verifying actual RTTs between node pairs..."
        echo "(Tip: pass a layout name to show expected values, e.g. 'verify global-spread')"
    fi
    echo "(Each pair sends 3 pings; showing avg RTT)"
    echo ""

    printf "  %-20s %-12s %-12s\n" "PAIR" "EXPECTED" "ACTUAL"
    printf "  %-20s %-12s %-12s\n" "----" "--------" "------"

    for i in "${!CONTAINERS[@]}"; do
        for j in "${!CONTAINERS[@]}"; do
            [[ "$i" -ge "$j" ]] && continue
            local src="${CONTAINERS[$i]}"
            local dst_ip="${IPS[$j]}"
            local expected="${LAYOUT[$i:$j]:-?}ms"

            # Ping 3 times, extract avg RTT
            local avg
            avg=$(docker exec "$src" ping -c 3 -W 2 "$dst_ip" 2>/dev/null | \
                  tail -1 | sed -E 's|.*= [0-9.]+/([0-9.]+)/.*|\1|' || echo "timeout")

            printf "  %-20s %-12s %-12s\n" \
                "${LABELS[$i]} ↔ ${LABELS[$j]}" \
                "$expected" \
                "${avg}ms"
        done
    done
}

cmd_verify_quick() {
    # Fast verification: check that TC netem rules are configured on each node and,
    # if ping is available, verify actual RTTs are within tolerance.
    # Returns non-zero if any pair fails. Intended for automated pipelines.
    local layout="${1:?Usage: verify-quick <layout>}"
    local tolerance_pct="${2:-50}"  # default ±50% tolerance
    load_layout "$layout"

    local failures=0
    local pairs_checked=0
    local has_ping="true"

    # Detect whether ping is available in the first container
    if ! docker exec "${CONTAINERS[0]}" which ping > /dev/null 2>&1; then
        has_ping="false"
        echo "  (ping not available in containers — verifying per-peer TC rules only)"
    fi

    for i in "${!CONTAINERS[@]}"; do
        local container="${CONTAINERS[$i]}"
        local qdisc_output
        qdisc_output=$(docker exec "$container" tc qdisc show dev eth0 2>&1)
        if ! echo "$qdisc_output" | grep -q "netem"; then
            echo "  FAIL: ${LABELS[$i]}: no netem qdisc found on eth0" >&2
            failures=$((failures + 1))
            continue
        fi

        local filter_output
        filter_output=$(docker exec "$container" tc filter show dev eth0 2>&1)

        for j in "${!CONTAINERS[@]}"; do
            [[ "$i" -eq "$j" ]] && continue

            local dst_ip="${IPS[$j]}"
            local dst_hex
            dst_hex="$(ip_to_tc_hex "$dst_ip")"
            local expected_rtt="${LAYOUT[$i:$j]}"
            local expected_one_way=$(( expected_rtt / 2 ))
            local lower_tc=$(( expected_one_way * 70 / 100 ))
            local upper_tc=$(( expected_one_way * 130 / 100 ))
            local flowid
            flowid=$(
                echo "$filter_output" | awk -v hex="${dst_hex}/ffffffff" '
                    $1 == "match" && $2 == hex { print prev; exit }
                    { prev = $0 }
                ' | sed -nE 's/.*flowid 1:([0-9]+).*/\1/p'
            )
            local delay_ms=""
            if [[ -n "$flowid" ]]; then
                delay_ms=$(echo "$qdisc_output" | sed -nE "s/.*parent 1:${flowid} .* delay ([0-9]+)ms.*/\\1/p" | head -1)
            fi

            if [[ -z "$flowid" || -z "$delay_ms" || "$delay_ms" -lt "$lower_tc" || "$delay_ms" -gt "$upper_tc" ]]; then
                echo "  FAIL: ${LABELS[$i]} → ${LABELS[$j]}: tc rules missing expected delay/filter (one-way ${expected_one_way}ms)" >&2
                failures=$((failures + 1))
                pairs_checked=$((pairs_checked + 1))
                continue
            fi

            if [[ "$has_ping" == "true" ]]; then
                local actual
                actual=$(docker exec "$container" ping -c 1 -W 3 "$dst_ip" 2>/dev/null | \
                         sed -nE 's|.*time=([0-9.]+) ms.*|\1|p')

                if [[ -z "$actual" ]]; then
                    echo "  FAIL: ${LABELS[$i]} → ${LABELS[$j]}: ping timeout (expected ~${expected_rtt}ms)" >&2
                    failures=$((failures + 1))
                else
                    local lower upper
                    lower=$(echo "$expected_rtt $tolerance_pct" | awk '{printf "%.1f", $1 * (1 - $2/100)}')
                    upper=$(echo "$expected_rtt $tolerance_pct" | awk '{printf "%.1f", $1 * (1 + $2/100)}')
                    local in_range
                    in_range=$(echo "$actual $lower $upper" | awk '{print ($1 >= $2 && $1 <= $3) ? "yes" : "no"}')

                    if [[ "$in_range" == "yes" ]]; then
                        echo "  OK:   ${LABELS[$i]} → ${LABELS[$j]}: ${actual}ms (expected ${expected_rtt}ms ±${tolerance_pct}%)"
                    else
                        echo "  FAIL: ${LABELS[$i]} → ${LABELS[$j]}: ${actual}ms (expected ${expected_rtt}ms ±${tolerance_pct}%, range ${lower}-${upper}ms)" >&2
                        failures=$((failures + 1))
                    fi
                fi
            else
                echo "  OK:   ${LABELS[$i]} → ${LABELS[$j]}: tc delay/filter configured"
            fi
            pairs_checked=$((pairs_checked + 1))
        done
    done

    echo "  Verified ${pairs_checked} directed paths, ${failures} failures."
    [[ "$failures" -eq 0 ]]
}

cmd_layouts() {
    echo "Available network topology layouts:"
    echo ""

    for layout in global-spread us-distributed eu-cluster se-asia-regional europe-regional south-america-regional degraded-wan intercontinental; do
        load_layout "$layout"
        echo "  ${LAYOUT_NAME}"
        echo "    ${LAYOUT_DESC}"
        echo "    Locations: ${LAYOUT_LOCATIONS[*]}"

        # Show min/max RTT
        local min=99999 max=0
        for i in 0 1 2 3; do
            for j in 0 1 2 3; do
                [[ "$i" -eq "$j" ]] && continue
                local rtt="${LAYOUT[$i:$j]}"
                [[ "$rtt" -lt "$min" ]] && min="$rtt"
                [[ "$rtt" -gt "$max" ]] && max="$rtt"
            done
        done
        echo "    RTT range: ${min}ms - ${max}ms"
        printf "    Link quality: jitter %s%% (cap %sms), loss %s%%/dir" \
            "${LAYOUT_JITTER_PCT}" "${LAYOUT_JITTER_CAP_MS}" "${LAYOUT_LOSS_PCT}"
        [[ "${LAYOUT_LOSS_CORR_PCT}" -gt 0 ]] && printf " (%s%% bursty)" "${LAYOUT_LOSS_CORR_PCT}"
        echo ""
        echo ""
    done
}

# ============================================================================
# Main
# ============================================================================

case "${1:-help}" in
    apply)        cmd_apply "${2:-global-spread}" ;;
    clear)        cmd_clear ;;
    status)       cmd_status ;;
    verify)       cmd_verify "${2:-}" ;;
    verify-quick) cmd_verify_quick "${2:-}" "${3:-50}" ;;
    layouts)      cmd_layouts ;;
    *)
        echo "Usage: $0 {apply|clear|status|verify|verify-quick|layouts}"
        echo ""
        echo "  apply <layout>        Apply a geographic network topology"
        echo "  clear                 Remove all tc rules (restore zero latency)"
        echo "  status                Show current tc rules per node"
        echo "  verify [<layout>]     Ping between all pairs to measure actual RTT"
        echo "  verify-quick <layout> Fast check: ping one pair/node, exit non-zero on failure"
        echo "  layouts               List available geographic layouts"
        echo ""
        echo "Environment overrides for non-Diesis chains:"
        echo "  NODE1_CONTAINER=sonic-node-1 NODE1_IP=... $0 apply global-spread"
        exit 1
        ;;
esac
