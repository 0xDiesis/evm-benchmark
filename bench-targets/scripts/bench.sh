#!/bin/bash
# bench.sh — Single benchmark run orchestrator.
# Runs one benchmark against one chain. Called by the Makefile and by
# compare.sh / sweep.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_TARGETS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
BENCH_REPO_DIR="$(cd "${BENCH_TARGETS_DIR}/.." && pwd)"
TOPOLOGY_SCRIPT="${BENCH_TARGETS_DIR}/network-topology/network-topology.sh"

# Source shared library
if [[ ! -f "${SCRIPT_DIR}/lib.sh" ]]; then
    echo "ERROR: lib.sh not found at ${SCRIPT_DIR}/lib.sh" >&2; exit 1
fi
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

# ── Defaults ──────────────────────────────────────────────────────────────
CHAIN="diesis" MODE="burst" ENV="clean" TAG=""
TXS=2000 TPS=200 DURATION=30 SENDERS=200 BATCH_SIZE=200 WORKERS=8
OUT="" QUIET="true" FUND="true" TEST_MODE="transfer" REBUILD="false" DEV="false"

# ── Argument parsing ─────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --chain)       CHAIN="$2";      shift 2 ;;
        --mode)        MODE="$2";       shift 2 ;;
        --env)         ENV="$2";        shift 2 ;;
        --tag)         TAG="$2";        shift 2 ;;
        --txs)         TXS="$2";        shift 2 ;;
        --tps)         TPS="$2";        shift 2 ;;
        --duration)    DURATION="$2";   shift 2 ;;
        --senders)     SENDERS="$2";    shift 2 ;;
        --batch-size)  BATCH_SIZE="$2"; shift 2 ;;
        --workers)     WORKERS="$2";    shift 2 ;;
        --out)         OUT="$2";        shift 2 ;;
        --quiet)       QUIET="true";    shift ;;
        --no-quiet)    QUIET="false";   shift ;;
        --no-fund)     FUND="false";    shift ;;
        --rebuild)     REBUILD="true";  shift ;;
        --dev)         DEV="true";       shift ;;
        --test-mode)   TEST_MODE="$2";  shift 2 ;;
        --help|-h)
            sed -n '2,4p' "$0"; echo ""
            echo "Usage: $0 [options]"
            echo "  --chain <name>       Chain (default: diesis)"
            echo "  --mode <mode>        burst|sustained|ceiling (default: burst)"
            echo "  --env <env>          clean|geo-global|geo-us|geo-eu|geo-degraded|geo-intercontinental (default: clean)"
            echo "  --tag <tag>          Custom run tag (default: value of --env)"
            echo "  --txs <n>            Burst tx count (default: 2000)"
            echo "  --tps <n>            Target TPS for sustained/ceiling (default: 200)"
            echo "  --duration <n>       Sustained duration in seconds (default: 30)"
            echo "  --senders <n>        Sender count (default: 200)"
            echo "  --batch-size <n>     RPC batch size (default: 200)"
            echo "  --workers <n>        Worker count (default: 8)"
            echo "  --out <path>         Override output directory"
            echo "  --quiet              Suppress harness output (default: true)"
            echo "  --no-fund            Skip auto-funding sender accounts"
            echo "  --dev                Use debug build image (faster compile, slower runtime)"
            echo "  --test-mode <mode>   transfer|evm (default: transfer)"
            exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

RUN_TAG="${TAG:-${ENV}}"

# ── Load chain config ────────────────────────────────────────────────────
if ! load_chain_config "${CHAIN}"; then
    echo "ERROR: Unknown or misconfigured chain '${CHAIN}'" >&2
    echo "Available:" >&2
    ls "${BENCH_TARGETS_DIR}/chains/" 2>/dev/null | sed 's/^/  /' >&2
    exit 1
fi

# ── Create run directory ─────────────────────────────────────────────────
if [[ -n "${OUT}" ]]; then RUN_DIR="${OUT}"
else RUN_DIR="$(make_run_dir "${CHAIN}" "${MODE}" "${RUN_TAG}")"; fi
mkdir -p "${RUN_DIR}"

# ── Topology cleanup trap ────────────────────────────────────────────────
TOPOLOGY_APPLIED="false"
cleanup() {
    if [[ "${TOPOLOGY_APPLIED}" == "true" && -x "${TOPOLOGY_SCRIPT}" ]]; then
        echo ""; echo "Cleaning up network topology..."
        eval "${CHAIN_TOPOLOGY_ENV:-}" bash "${TOPOLOGY_SCRIPT}" clear 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ── Map --env to topology layout ─────────────────────────────────────────
env_to_layout() {
    case "$1" in
        geo-global)           echo "global-spread" ;;
        geo-us)               echo "us-distributed" ;;
        geo-eu)               echo "eu-cluster" ;;
        geo-degraded)         echo "degraded-wan" ;;
        geo-intercontinental) echo "intercontinental" ;;
        *)                    echo "" ;;
    esac
}

default_chain_overrides() {
    local chain_name="$1"

    case "${chain_name}" in
        diesis)
            echo "E2E_BLOCK_PERIOD=100ms E2E_ORDERING_WINDOW=15ms E2E_MIN_ROUND_DELAY=10ms E2E_MAX_EXECUTION_LAG=32 E2E_PROPAGATION_DELAY_STOP_THRESHOLD=40 E2E_MAX_PROPOSAL_TX_COUNT=2048 E2E_MAX_GAS_PER_PROPOSAL=30000000"
            ;;
        *)
            echo ""
            ;;
    esac
}

# ── Geo-aware chain config ──────────────────────────────────────────────
# Consensus requires block_period > max inter-node RTT to maintain liveness.
# Override timing parameters when running under simulated geo-latency.
geo_chain_overrides() {
    local env_name="$1"
    local block_period="" ordering_window="" min_round_delay="" leader_timeout="" prop_threshold=""

    # Use the same consensus parameters for ALL geo profiles so that cross-profile
    # comparisons are fair (Sei/Sonic also run identical configs across profiles).
    # The node defaults (1s / 300ms / 120ms / 40) are tuned for worst-case geo-global
    # (240ms max RTT) and work safely for all lower-latency profiles too.
    case "${env_name}" in
        geo-eu|geo-us|geo-global|geo-degraded)
            block_period="1s"; ordering_window="300ms"; min_round_delay="120ms"
            prop_threshold="40" ;;
        geo-intercontinental)
            # Max RTT 340ms exceeds the global defaults — widen margins.
            block_period="2s"; ordering_window="500ms"; min_round_delay="200ms"
            prop_threshold="50" ;;
        *)
            echo ""; return ;;
    esac

    # Optional tuning overrides for geo sweeps.
    # These preserve per-env defaults when not supplied.
    block_period="${BENCH_GEO_E2E_BLOCK_PERIOD:-${block_period}}"
    ordering_window="${BENCH_GEO_E2E_ORDERING_WINDOW:-${ordering_window}}"
    min_round_delay="${BENCH_GEO_E2E_MIN_ROUND_DELAY:-${min_round_delay}}"
    leader_timeout="${BENCH_GEO_E2E_LEADER_TIMEOUT:-${leader_timeout}}"
    prop_threshold="${BENCH_GEO_E2E_PROPAGATION_DELAY_STOP_THRESHOLD:-${prop_threshold}}"

    local overrides="E2E_BLOCK_PERIOD=${block_period} E2E_ORDERING_WINDOW=${ordering_window} E2E_MIN_ROUND_DELAY=${min_round_delay}"
    [[ -n "${leader_timeout}" ]] && overrides+=" E2E_LEADER_TIMEOUT=${leader_timeout}"
    [[ -n "${prop_threshold}" ]] && overrides+=" E2E_PROPAGATION_DELAY_STOP_THRESHOLD=${prop_threshold}"

    [[ -n "${BENCH_GEO_E2E_MAX_PROPOSAL_TX_COUNT:-}" ]] && overrides+=" E2E_MAX_PROPOSAL_TX_COUNT=${BENCH_GEO_E2E_MAX_PROPOSAL_TX_COUNT}"
    [[ -n "${BENCH_GEO_E2E_MAX_GAS_PER_PROPOSAL:-}" ]] && overrides+=" E2E_MAX_GAS_PER_PROPOSAL=${BENCH_GEO_E2E_MAX_GAS_PER_PROPOSAL}"
    [[ -n "${BENCH_GEO_E2E_FEC_REDUNDANCY_RATIO:-}" ]] && overrides+=" E2E_FEC_REDUNDANCY_RATIO=${BENCH_GEO_E2E_FEC_REDUNDANCY_RATIO}"
    [[ -n "${BENCH_GEO_E2E_FEC_MIN_MESSAGE_SIZE:-}" ]] && overrides+=" E2E_FEC_MIN_MESSAGE_SIZE=${BENCH_GEO_E2E_FEC_MIN_MESSAGE_SIZE}"

    echo "${overrides}"
}

# ── Restart chain for consistent baseline ────────────────────────────────
# Always restart to ensure a clean state between runs. This prevents warm-cache
# effects and leftover TC rules from skewing results across profiles.
BASE_OVERRIDES="$(default_chain_overrides "${CHAIN}")"
GEO_OVERRIDES=""
if [[ "${CHAIN}" == "diesis" ]]; then
    GEO_OVERRIDES="$(geo_chain_overrides "${ENV}")"
fi
CHAIN_OVERRIDES="${BASE_OVERRIDES}"
if [[ -n "${GEO_OVERRIDES}" ]]; then
    CHAIN_OVERRIDES="${CHAIN_OVERRIDES} ${GEO_OVERRIDES}"
fi
if [[ "${REBUILD}" == "true" && -n "${CHAIN_REBUILD_CMD:-}" ]]; then
    if [[ "${DEV}" == "true" && -n "${CHAIN_REBUILD_DEV_CMD:-}" ]]; then
        # Debug builds skip expensive verkle computation so blocks execute fast
        # enough to meet deploy timeouts.  QUIC/FEC behavior is unaffected.
        DEV_OVERRIDES="E2E_COMMITMENT_MODE=none E2E_SKIP_STATE_ROOT_VALIDATION=true"
        echo "Rebuilding and restarting ${CHAIN} (dev/debug build — faster compile)..."
        eval "${CHAIN_DOWN_CMD}" 2>/dev/null || true
        eval "${CHAIN_OVERRIDES} ${DEV_OVERRIDES} ${CHAIN_REBUILD_DEV_CMD}" || { echo "ERROR: Dev rebuild failed" >&2; exit 1; }
    else
        echo "Rebuilding and restarting ${CHAIN} (forced rebuild)..."
        eval "${CHAIN_DOWN_CMD}" 2>/dev/null || true
        eval "${CHAIN_OVERRIDES} ${CHAIN_REBUILD_CMD}" || { echo "ERROR: Rebuild failed" >&2; exit 1; }
    fi
elif [[ -n "${CHAIN_CLEAN_CMD:-}" ]]; then
    echo "Restarting ${CHAIN} from clean state..."
    eval "${CHAIN_DOWN_CMD}" 2>/dev/null || true
    eval "${CHAIN_OVERRIDES} ${CHAIN_UP_CMD}" || { echo "ERROR: Restart failed" >&2; exit 1; }
fi

# ── Reload config after restart (picks up dynamic ports, e.g. Berachain) ─
load_chain_config "${CHAIN}"

# ── Wait for chain readiness ─────────────────────────────────────────────
echo "Waiting for ${CHAIN} to be ready..."
FIRST_RPC="${CHAIN_RPC%%,*}"
CHAIN_READY_TIMEOUT_SECS="${BENCH_CHAIN_READY_TIMEOUT_SECS:-120}"
if ! wait_for_chain "${FIRST_RPC}" "${CHAIN_READY_TIMEOUT_SECS}"; then
    echo "ERROR: ${CHAIN} did not become ready in time" >&2; exit 1
fi
# Wait for consensus to be actively producing blocks before benchmarking.
# This prevents false-fast readiness where the RPC responds but the chain
# has not yet established quorum after a fresh restart.
# Always wait after a clean restart; skip only if chain was not restarted.
_BLOCK_ADVANCE_TIMEOUT="${BENCH_BLOCK_ADVANCE_TIMEOUT_SECS:-60}"
wait_for_block_advance "${FIRST_RPC}" "${_BLOCK_ADVANCE_TIMEOUT}"

# ── Apply network topology if geo-* ─────────────────────────────────────
TOPOLOGY_LAYOUT="$(env_to_layout "${ENV}")"
if [[ -n "${TOPOLOGY_LAYOUT}" ]]; then
    if [[ ! -x "${TOPOLOGY_SCRIPT}" ]]; then
        echo "ERROR: Topology script not found at ${TOPOLOGY_SCRIPT}" >&2; exit 1
    fi
    if [[ -z "${CHAIN_TOPOLOGY_ENV:-}" ]]; then
        echo "WARNING: ${CHAIN} has no topology mapping — geo latency not applied (single-node chain)."
        echo "  Running ${ENV} benchmark without network simulation."
        TOPOLOGY_LAYOUT=""
    fi
fi
if [[ -n "${TOPOLOGY_LAYOUT}" ]]; then
    echo "Applying network topology: ${TOPOLOGY_LAYOUT} (${CHAIN})"
    eval "${CHAIN_TOPOLOGY_ENV:-}" bash "${TOPOLOGY_SCRIPT}" apply "${TOPOLOGY_LAYOUT}"
    TOPOLOGY_APPLIED="true"
    sleep 2  # let tc rules stabilize

    # Verify that TC latency is actually working before running the benchmark.
    # This catches silent failures (wrong interface, missing kernel module, etc.)
    echo "Verifying network topology..."
    if ! eval "${CHAIN_TOPOLOGY_ENV:-}" bash "${TOPOLOGY_SCRIPT}" verify-quick "${TOPOLOGY_LAYOUT}"; then
        echo "ERROR: Network topology verification failed for ${CHAIN}." >&2
        echo "  TC rules may not have applied correctly. Check container capabilities (NET_ADMIN)," >&2
        echo "  iproute2 installation, and kernel netem module." >&2
        exit 1
    fi
fi

# ── Generate metadata ────────────────────────────────────────────────────
if [[ -f "${SCRIPT_DIR}/meta.sh" ]]; then
    # shellcheck source=meta.sh
    source "${SCRIPT_DIR}/meta.sh"
    export BENCH_TXS="${TXS}" BENCH_SENDERS="${SENDERS}" BENCH_BATCH_SIZE="${BATCH_SIZE}"
    export BENCH_WORKERS="${WORKERS}" BENCH_TPS="${TPS}" BENCH_DURATION="${DURATION}"
    if [[ -n "${CHAIN_OVERRIDES}" ]]; then
        eval "export ${CHAIN_OVERRIDES}"
    fi
    generate_meta "${RUN_DIR}/meta.json" "${CHAIN}" "${MODE}" "${ENV}" "${RUN_TAG}" 2>/dev/null || true
fi

# ── Build harness if needed ──────────────────────────────────────────────
HARNESS="${BENCH_REPO_DIR}/target/release/evm-benchmark"
if [[ ! -f "${HARNESS}" ]]; then
    echo "Building evm-benchmark (release)..."
    cargo build -p evm-benchmark --release \
        --manifest-path "${BENCH_REPO_DIR}/Cargo.toml" 2>&1 | tail -3
fi

# ── Construct harness CLI args ───────────────────────────────────────────
HARNESS_ARGS=(
    --rpc-endpoints "${CHAIN_RPC}" --ws "${CHAIN_WS}"
    --chain-id "${CHAIN_CHAIN_ID}" --bench-name "${CHAIN}_${MODE}"
    --senders "${SENDERS}" --workers "${WORKERS}"
    --batch-size "${BATCH_SIZE}" --test "${TEST_MODE}"
    --out "${RUN_DIR}/report.json"
)
[[ "${FUND}" == "true" ]]  && HARNESS_ARGS+=(--fund)
[[ "${QUIET}" == "true" ]] && HARNESS_ARGS+=(--quiet)

case "${MODE}" in
    burst)
        HARNESS_ARGS+=(--execution burst --txs "${TXS}" --waves 8) ;;
    sustained)
        HARNESS_ARGS+=(--execution sustained --tps "${TPS}"
            --duration "${DURATION}") ;;
    ceiling)
        # Default ceiling isolation: restart between ramp steps to avoid
        # carry-over pending transactions skewing higher-load measurements.
        if [[ -z "${BENCH_CEILING_RESTART_CMD:-}" && -n "${CHAIN_CLEAN_CMD:-}" ]]; then
            export BENCH_CEILING_RESTART_CMD="${CHAIN_CLEAN_CMD}"
        fi
        export BENCH_CEILING_RESTART_BETWEEN_STEPS="${BENCH_CEILING_RESTART_BETWEEN_STEPS:-true}"
        export BENCH_CEILING_COOLDOWN_SECS="${BENCH_CEILING_COOLDOWN_SECS:-2}"
        export BENCH_CEILING_WARMUP_SECS="${BENCH_CEILING_WARMUP_SECS:-3}"
        export BENCH_CEILING_RESTART_READY_TIMEOUT_SECS="${BENCH_CEILING_RESTART_READY_TIMEOUT_SECS:-90}"
        HARNESS_ARGS+=(--execution ceiling --tps "${TPS}") ;;
    *)  echo "ERROR: Unknown mode '${MODE}'. Use: burst, sustained, ceiling" >&2; exit 1 ;;
esac

# ── Run harness ──────────────────────────────────────────────────────────
echo ""
echo "Running ${CHAIN} ${MODE} benchmark (env=${ENV})..."
echo "  RPC:     ${CHAIN_RPC}"
echo "  Senders: ${SENDERS}"
echo "  Output:  ${RUN_DIR}/report.json"
echo ""

if [[ -n "${CHAIN_KEYS:-}" ]]; then
    BENCH_KEY="${CHAIN_KEYS}" "${HARNESS}" "${HARNESS_ARGS[@]}" 2>&1 \
        | tee "${RUN_DIR}/console.log"
else
    "${HARNESS}" "${HARNESS_ARGS[@]}" 2>&1 \
        | tee "${RUN_DIR}/console.log"
fi
HARNESS_EXIT=${PIPESTATUS[0]}

if [[ "${HARNESS_EXIT}" -ne 0 ]]; then
    echo "ERROR: Harness exited with code ${HARNESS_EXIT}" >&2
fi

# ── Clear topology if applied ────────────────────────────────────────────
if [[ "${TOPOLOGY_APPLIED}" == "true" ]]; then
    echo ""; echo "Clearing network topology..."
    eval "${CHAIN_TOPOLOGY_ENV:-}" bash "${TOPOLOGY_SCRIPT}" clear
    TOPOLOGY_APPLIED="false"
fi

# ── Update latest symlinks ───────────────────────────────────────────────
update_latest_symlink "${RUN_DIR}"  # mode-level latest
MODE_DIR="$(dirname "${RUN_DIR}")"
CHAIN_DIR="$(dirname "${MODE_DIR}")"
RUNS_DIR="$(dirname "${CHAIN_DIR}")"
ln -sfn "${MODE_DIR}" "${CHAIN_DIR}/latest"
ln -sfn "${CHAIN_DIR}" "${RUNS_DIR}/latest"

# ── Extract and print key metrics ────────────────────────────────────────
if [[ -f "${RUN_DIR}/report.json" ]]; then
    echo ""
    echo "--- Results: ${CHAIN} ${MODE} (${ENV}) ---"
    python3 -c "
import json, sys
try:
    with open('${RUN_DIR}/report.json') as f:
        r = json.load(f)['results']
    print(f'  Submitted:     {r[\"submitted\"]}')
    print(f'  Confirmed:     {r[\"confirmed\"]}')
    print(f'  Confirmed TPS: {r[\"confirmed_tps\"]:.1f}')
    print(f'  Latency p50:   {r[\"latency\"][\"p50\"]}ms')
    print(f'  Latency p95:   {r[\"latency\"][\"p95\"]}ms')
    print(f'  Latency p99:   {r[\"latency\"][\"p99\"]}ms')
except Exception as e:
    print(f'  (report parsing failed: {e})', file=sys.stderr)
" 2>&1
fi

# ── Update index ─────────────────────────────────────────────────────────
if type -t index_add_run &>/dev/null && [[ -f "${RUN_DIR}/report.json" ]]; then
    read -r IDX_TPS IDX_P50 IDX_P99 IDX_RATE < <(python3 -c "
import json
with open('${RUN_DIR}/report.json') as f:
    r = json.load(f)['results']
    confirmed_rate = r['confirmed'] / r['submitted'] if r['submitted'] > 0 else 0
    print(f\"{r['confirmed_tps']:.1f} {r['latency']['p50']} {r['latency']['p99']} {confirmed_rate:.4f}\")
" 2>/dev/null) || true
    index_add_run "${RUN_DIR}" "${CHAIN}" "${MODE}" "${ENV}" \
        "${IDX_TPS:-0}" "${IDX_P50:-0}" "${IDX_P99:-0}" "${IDX_RATE:-0}" 2>/dev/null || true
fi

# ── Done ─────────────────────────────────────────────────────────────────
echo ""
echo "Run directory: ${RUN_DIR}"
exit "${HARNESS_EXIT:-0}"
