#!/bin/bash
set -euo pipefail

# run-comparison-bench.sh — Run benchmarks on Diesis and Sonic under the same
#                           network topology to compare throughput.
#
# Usage:
#   ./run-comparison-bench.sh [layout]
#
# Default layout: global-spread

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_REPO="$(cd "${SCRIPT_DIR}/../.." && pwd)"
# DIESIS_REPO_DIR points at the (private) Diesis source repo for compose files and
# cluster lifecycle. Falls back to the conventional sibling layout when unset.
DIESIS_REPO_DIR="${DIESIS_REPO_DIR:-${BENCH_REPO}/../diesis}"
if [[ ! -d "${DIESIS_REPO_DIR}" ]]; then
    echo "ERROR: this script requires the Diesis source repo at \${DIESIS_REPO_DIR}." >&2
    echo "       Not found at: ${DIESIS_REPO_DIR}" >&2
    echo "       Set DIESIS_REPO_DIR to a Diesis checkout and re-run." >&2
    exit 2
fi
TOPOLOGY_SCRIPT="${SCRIPT_DIR}/network-topology.sh"
LAYOUT="${1:-global-spread}"
RESULTS_DIR="${BENCH_REPO}/bench-targets/results/topology-bench"

mkdir -p "${RESULTS_DIR}"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

echo "============================================================"
echo "  Network Topology Benchmark Comparison"
echo "  Layout: ${LAYOUT}"
echo "  Date:   $(date)"
echo "============================================================"
echo ""

# --- Benchmark helper ---
run_harness() {
    local chain="$1"
    local label="$2"
    local rpc="$3"
    local ws="$4"
    local chain_id="$5"
    local bench_key="$6"
    shift 6
    local out="${RESULTS_DIR}/${TIMESTAMP}-${chain}-${label}.json"

    echo "  Running: ${chain} / ${label} ..."
    BENCH_KEY="${bench_key}" \
        cargo run --release --manifest-path "${BENCH_REPO}/crates/evm-benchmark/Cargo.toml" -- \
        --rpc-endpoints "${rpc}" \
        --ws "${ws}" \
        --chain-id "${chain_id}" \
        --bench-name "${chain}_${LAYOUT}_${label}" \
        --out "${out}" \
        "$@" 2>&1 | tail -5

    echo "  Result: ${out}"
    echo ""
}

# ============================================================
# PHASE 1: Diesis
# ============================================================
echo ">>> PHASE 1: Diesis benchmarks"
echo ""

# Check Diesis is running
if ! curl -sf http://localhost:8545 -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' >/dev/null 2>&1; then
    echo "ERROR: Diesis e2e cluster not running on :8545"
    exit 1
fi

# Verify tc is available
if ! docker exec diesis-e2e-node-1 tc --version >/dev/null 2>&1; then
    echo "ERROR: tc not available in Diesis containers. Rebuild with iproute2."
    exit 1
fi

# Apply topology
echo "Applying ${LAYOUT} topology to Diesis..."
bash "${TOPOLOGY_SCRIPT}" apply "${LAYOUT}"
echo ""

DIESIS_RPC="http://localhost:8545,http://localhost:8555,http://localhost:8565,http://localhost:8575"
DIESIS_WS="ws://localhost:8546"
DIESIS_CHAIN_ID="19803"
DIESIS_KEY="0x0000000000000000000000000000000000000000000000000000000000000001,0x0000000000000000000000000000000000000000000000000000000000000002,0x0000000000000000000000000000000000000000000000000000000000000003,0x0000000000000000000000000000000000000000000000000000000000000004"

# Test 1: Burst 1000 simple transfers
run_harness diesis burst-1k "${DIESIS_RPC}" "${DIESIS_WS}" "${DIESIS_CHAIN_ID}" "${DIESIS_KEY}" \
    --txs 1000 --execution burst

# Test 2: Sustained 300 TPS for 20s
run_harness diesis sustained-300 "${DIESIS_RPC}" "${DIESIS_WS}" "${DIESIS_CHAIN_ID}" "${DIESIS_KEY}" \
    --execution sustained --tps 300 --duration 20

# Test 3: Ceiling (find max TPS)
run_harness diesis ceiling "${DIESIS_RPC}" "${DIESIS_WS}" "${DIESIS_CHAIN_ID}" "${DIESIS_KEY}" \
    --execution ceiling --tps 200

# Clear topology
echo "Clearing Diesis topology..."
bash "${TOPOLOGY_SCRIPT}" clear
echo ""

# Stop Diesis
echo "Stopping Diesis cluster..."
cd "${DIESIS_REPO_DIR}" && make e2e-down 2>&1 | tail -2
echo ""

# ============================================================
# PHASE 2: Sonic
# ============================================================
echo ">>> PHASE 2: Sonic benchmarks"
echo ""

SONIC_DIR="${BENCH_REPO}/bench-targets/chains/sonic"

# Start Sonic
echo "Starting Sonic cluster..."
cd "${SONIC_DIR}" && make up 2>&1 | tail -5
echo ""

# Apply topology (Sonic IPs)
echo "Applying ${LAYOUT} topology to Sonic..."
NODE1_CONTAINER=sonic-node-1 NODE1_IP=10.101.0.11 \
NODE2_CONTAINER=sonic-node-2 NODE2_IP=10.101.0.12 \
NODE3_CONTAINER=sonic-node-3 NODE3_IP=10.101.0.13 \
NODE4_CONTAINER=sonic-node-4 NODE4_IP=10.101.0.14 \
bash "${TOPOLOGY_SCRIPT}" apply "${LAYOUT}"
echo ""

SONIC_RPC="http://localhost:18545,http://localhost:18645,http://localhost:18745,http://localhost:18845"
SONIC_WS="ws://localhost:18546"
SONIC_CHAIN_ID="4003"
# Sonic uses validator keys from genesis — use the same funding key approach
SONIC_KEY="0xb71c71a67e1177ad4e901695e1b4b9ee17ae16c6668d313eac2f96dbcda3f291"

# Fund accounts first
echo "Funding Sonic accounts..."
cd "${SONIC_DIR}" && make fund 2>&1 | tail -3
echo ""

# Test 1: Burst 1000 simple transfers
run_harness sonic burst-1k "${SONIC_RPC}" "${SONIC_WS}" "${SONIC_CHAIN_ID}" "${SONIC_KEY}" \
    --txs 1000 --execution burst

# Test 2: Sustained 300 TPS for 20s
run_harness sonic sustained-300 "${SONIC_RPC}" "${SONIC_WS}" "${SONIC_CHAIN_ID}" "${SONIC_KEY}" \
    --execution sustained --tps 300 --duration 20

# Test 3: Ceiling (find max TPS)
run_harness sonic ceiling "${SONIC_RPC}" "${SONIC_WS}" "${SONIC_CHAIN_ID}" "${SONIC_KEY}" \
    --execution ceiling --tps 200

# Clear topology and stop Sonic
echo "Clearing Sonic topology..."
NODE1_CONTAINER=sonic-node-1 NODE1_IP=10.101.0.11 \
NODE2_CONTAINER=sonic-node-2 NODE2_IP=10.101.0.12 \
NODE3_CONTAINER=sonic-node-3 NODE3_IP=10.101.0.13 \
NODE4_CONTAINER=sonic-node-4 NODE4_IP=10.101.0.14 \
bash "${TOPOLOGY_SCRIPT}" clear

echo "Stopping Sonic cluster..."
cd "${SONIC_DIR}" && make down 2>&1 | tail -2
echo ""

# ============================================================
# SUMMARY
# ============================================================
echo "============================================================"
echo "  Benchmark Complete"
echo "  Results in: ${RESULTS_DIR}/"
echo "============================================================"
echo ""
ls -la "${RESULTS_DIR}/${TIMESTAMP}"*.json 2>/dev/null || echo "(no JSON results found)"
