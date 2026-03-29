#!/bin/bash
# lib.sh — Shared shell library for the benchmark suite.
# Sourced by all other scripts. Provides constants, logging, chain registry,
# results directory helpers, index management, and topology wrappers.
set -euo pipefail

# ── 1. Constants ───────────────────────────────────────────────────────────────
BENCH_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIESIS_REPO_DIR="${DIESIS_REPO_DIR:-${BENCH_REPO_ROOT}/../diesis}"
RESULTS_BASE="${BENCH_REPO_ROOT}/bench-targets/results"
HARNESS_MANIFEST="${BENCH_REPO_ROOT}/crates/evm-benchmark/Cargo.toml"
TOPOLOGY_SCRIPT="${BENCH_REPO_ROOT}/bench-targets/network-topology/network-topology.sh"

# ── Docker image tagging ──────────────────────────────────────────────────────
# Auto-derive image tag from the Diesis repo's git short SHA.
# Override with DIESIS_IMAGE_TAG=mytag to use a specific tag.
# This enables instant switching between branches for A/B benchmarking.
diesis_image_tag() {
    if [[ -n "${DIESIS_IMAGE_TAG:-}" ]]; then
        echo "${DIESIS_IMAGE_TAG}"
    elif [[ -d "${DIESIS_REPO_DIR}/.git" || -f "${DIESIS_REPO_DIR}/.git" ]]; then
        local branch dirty_suffix=""
        # Use branch name as tag (e.g. main, feat-pqc-ml-dsa)
        # Slashes are replaced with dashes for Docker tag compatibility.
        branch="$(git -C "${DIESIS_REPO_DIR}" branch --show-current 2>/dev/null | tr '/' '-')"
        branch="${branch:-latest}"
        # Append -dirty if there are uncommitted changes to crates/ or docker/
        # so the image is rebuilt when source files change without a commit.
        if ! git -C "${DIESIS_REPO_DIR}" diff --quiet -- crates/ docker/ 2>/dev/null; then
            dirty_suffix="-dirty"
        fi
        echo "${branch}${dirty_suffix}"
    else
        echo "latest"
    fi
}

# Check if a tagged Docker image exists locally.
diesis_image_exists() {
    local tag="${1:-$(diesis_image_tag)}"
    docker image inspect "diesis-node-e2e:${tag}" >/dev/null 2>&1
}

# ── 2. Logging functions ──────────────────────────────────────────────────────
_CLR_RESET="\033[0m"
_CLR_BLUE="\033[1;34m"
_CLR_YELLOW="\033[1;33m"
_CLR_RED="\033[1;31m"
_CLR_CYAN="\033[1;36m"
_CLR_GREEN="\033[1;32m"

log_info()   { echo -e "[$(date +%H:%M:%S)] ${_CLR_BLUE}INFO${_CLR_RESET}  $*"; }
log_warn()   { echo -e "[$(date +%H:%M:%S)] ${_CLR_YELLOW}WARN${_CLR_RESET}  $*" >&2; }
log_error()  { echo -e "[$(date +%H:%M:%S)] ${_CLR_RED}ERROR${_CLR_RESET} $*" >&2; }

log_header() {
    local msg="$1"
    local width=$(( ${#msg} + 6 ))
    local border
    border=$(printf '═%.0s' $(seq 1 "$width"))
    echo ""
    echo -e "${_CLR_CYAN}${border}${_CLR_RESET}"
    echo -e "${_CLR_CYAN}   ${msg}${_CLR_RESET}"
    echo -e "${_CLR_CYAN}${border}${_CLR_RESET}"
    echo ""
}

log_metric() {
    local key="$1" value="$2"
    echo -e "  ${_CLR_GREEN}${key}${_CLR_RESET}=${value}"
}

# ── 3. RPC health check ──────────────────────────────────────────────────────
check_rpc() {
    local url="$1"
    curl -sf "$url" -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        > /dev/null 2>&1
}

wait_for_chain() {
    local url="$1"
    local timeout="${2:-60}"
    local elapsed=0
    local interval=2

    log_info "Waiting for chain at ${url} (timeout ${timeout}s)..."
    while ! check_rpc "$url"; do
        elapsed=$((elapsed + interval))
        if [ "$elapsed" -ge "$timeout" ]; then
            log_error "Chain at ${url} did not become ready within ${timeout}s"
            return 1
        fi
        sleep "$interval"
    done
    log_info "Chain at ${url} is ready (${elapsed}s)"
    return 0
}

# Wait for the chain to advance by at least N blocks from the current height.
# This ensures consensus is stably producing blocks before benchmarking, not
# just responding to RPC. Default waits for 3 blocks to confirm steady state.
wait_for_block_advance() {
    local url="$1"
    local timeout="${2:-60}"
    local min_advance="${3:-3}"
    local elapsed=0
    local interval=2

    local initial_block
    initial_block=$(curl -sf "$url" -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
        python3 -c "import sys,json; print(int(json.load(sys.stdin).get('result','0x0'),16))" 2>/dev/null || echo "0")
    local target_block=$(( initial_block + min_advance ))

    log_info "Waiting for ${url} to reach block ${target_block} (from ${initial_block})..."
    while true; do
        local current_block
        current_block=$(curl -sf "$url" -X POST -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
            python3 -c "import sys,json; print(int(json.load(sys.stdin).get('result','0x0'),16))" 2>/dev/null || echo "0")

        if [[ "$current_block" -ge "$target_block" ]]; then
            log_info "Chain is producing blocks steadily (block ${current_block})"
            return 0
        fi
        elapsed=$((elapsed + interval))
        if [ "$elapsed" -ge "$timeout" ]; then
            log_warn "Chain only reached block ${current_block} (target ${target_block}) within ${timeout}s — proceeding anyway"
            return 0
        fi
        sleep "$interval"
    done
}

# ── 4. Chain registry ────────────────────────────────────────────────────────
# Each chain_config_* function sets: CHAIN_RPC, CHAIN_WS, CHAIN_CHAIN_ID,
# CHAIN_KEYS, CHAIN_UP_CMD, CHAIN_DOWN_CMD, CHAIN_CLEAN_CMD,
# CHAIN_STATUS_CMD, CHAIN_TYPE.

_REGISTERED_CHAINS="diesis sonic sei avalanche anvil geth reth berachain bsc cosmos"

chain_config_diesis() {
    CHAIN_RPC="http://localhost:8545,http://localhost:8555,http://localhost:8565,http://localhost:8575"
    CHAIN_WS="ws://localhost:8546"
    CHAIN_CHAIN_ID=19803
    CHAIN_KEYS="0x0000000000000000000000000000000000000000000000000000000000000001,0x0000000000000000000000000000000000000000000000000000000000000002,0x0000000000000000000000000000000000000000000000000000000000000003,0x0000000000000000000000000000000000000000000000000000000000000004"
    local _tag
    _tag="$(diesis_image_tag)"
    # e2e-up auto-detects: reuses cached image if it exists, builds release if not.
    # e2e-rebuild forces a fresh build (use when you need to pick up code changes).
    # e2e-rebuild-dev forces a fresh debug build (faster compile, use for troubleshooting).
    CHAIN_UP_CMD="make -C ${DIESIS_REPO_DIR} e2e-up E2E_IMAGE_TAG=${_tag}"
    CHAIN_DOWN_CMD="make -C ${DIESIS_REPO_DIR} e2e-down E2E_IMAGE_TAG=${_tag}"
    CHAIN_REBUILD_CMD="make -C ${DIESIS_REPO_DIR} e2e-rebuild E2E_IMAGE_TAG=${_tag}"
    CHAIN_REBUILD_DEV_CMD="make -C ${DIESIS_REPO_DIR} e2e-rebuild-dev E2E_IMAGE_TAG=${_tag}-dev"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:8545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="L1 (Mysticeti BFT, 4 validators)"
    # Topology overrides — container names and static IPs for network-topology.sh
    CHAIN_TOPOLOGY_ENV="NODE1_CONTAINER=diesis-e2e-node-1 NODE1_IP=10.100.0.11 NODE2_CONTAINER=diesis-e2e-node-2 NODE2_IP=10.100.0.12 NODE3_CONTAINER=diesis-e2e-node-3 NODE3_IP=10.100.0.13 NODE4_CONTAINER=diesis-e2e-node-4 NODE4_IP=10.100.0.14"
}

chain_config_sonic() {
    local sonic_dir="${BENCH_REPO_ROOT}/bench-targets/chains/sonic"
    CHAIN_RPC="http://localhost:18545,http://localhost:18645,http://localhost:18745,http://localhost:18845"
    CHAIN_WS="ws://localhost:18546"
    CHAIN_CHAIN_ID=4003
    CHAIN_KEYS="0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7,0x3144c0aa4ced56dc15c79b045bc5559a5ac9363d98db6df321fe3847a103740f,0x04a531f967898df5dbe223b67989b248e23c1c356a3f6717775cccb7fe53482c,0x00ca81d4fe11c23fae8b5e5b06f9fe952c99ca46abaec8bda70a678cd0314dde"
    CHAIN_UP_CMD="make -C ${sonic_dir} up"
    CHAIN_DOWN_CMD="make -C ${sonic_dir} down"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:18545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="L1 (Lachesis aBFT, 4 validators)"
    CHAIN_TOPOLOGY_ENV="NODE1_CONTAINER=sonic-node-1 NODE1_IP=10.101.0.11 NODE2_CONTAINER=sonic-node-2 NODE2_IP=10.101.0.12 NODE3_CONTAINER=sonic-node-3 NODE3_IP=10.101.0.13 NODE4_CONTAINER=sonic-node-4 NODE4_IP=10.101.0.14"
}

chain_config_sei() {
    local sei_dir="${BENCH_REPO_ROOT}/bench-targets/chains/sei"
    CHAIN_RPC="http://localhost:28545,http://localhost:28547,http://localhost:28549,http://localhost:28551"
    CHAIN_WS="ws://localhost:28546"
    CHAIN_CHAIN_ID=713714
    CHAIN_KEYS=""
    CHAIN_UP_CMD="make -C ${sei_dir} up"
    CHAIN_DOWN_CMD="make -C ${sei_dir} down"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:28545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="L1 (sei-tendermint BFT, 4 validators)"
    CHAIN_TOPOLOGY_ENV="NODE1_CONTAINER=sei-node-0 NODE1_IP=192.168.20.10 NODE2_CONTAINER=sei-node-1 NODE2_IP=192.168.20.11 NODE3_CONTAINER=sei-node-2 NODE3_IP=192.168.20.12 NODE4_CONTAINER=sei-node-3 NODE4_IP=192.168.20.13"
}

chain_config_avalanche() {
    local avalanche_dir="${BENCH_REPO_ROOT}/bench-targets/chains/avalanche"
    CHAIN_RPC="http://localhost:9650/ext/bc/C/rpc"
    CHAIN_WS="ws://localhost:9650/ext/bc/C/ws"
    CHAIN_CHAIN_ID=43112
    CHAIN_KEYS="0x56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027"
    CHAIN_UP_CMD="make -C ${avalanche_dir} up"
    CHAIN_DOWN_CMD="make -C ${avalanche_dir} down"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:9650/ext/bc/C/rpc -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="L1 (Snowman consensus, 5 nodes)"
}

chain_config_bsc() {
    local bsc_dir="${BENCH_REPO_ROOT}/bench-targets/chains/bsc"
    CHAIN_RPC="http://localhost:8545"
    CHAIN_WS="ws://localhost:8546"
    CHAIN_CHAIN_ID=714714
    CHAIN_KEYS="0xb5a03afd7e912d137a7ec5e824c0aacba543a455c618acdf46843b9890087bca,0x6e36ad7e68ae0b565970cc55c8fa4c69ba753e9a9210d80b8030b6db6eceb667,0x5177497e2c518042c97b07ef08839f60b88bf3437ad93c83f4dfe66b1fd06014,0x0b48828bc5954f830b5ed3dcaed08ffad1f76a27735d877fa262fe26a92a353a"
    CHAIN_UP_CMD="make -C ${bsc_dir} up"
    CHAIN_DOWN_CMD="make -C ${bsc_dir} down"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:8545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="L1 (BSC local cluster)"
    # 3 validators + rpc node; apply TC to the 3 consensus-participating validators
    CHAIN_TOPOLOGY_ENV="NODE1_CONTAINER=bsc-validator-1 NODE1_IP=10.102.0.11 NODE2_CONTAINER=bsc-validator-2 NODE2_IP=10.102.0.12 NODE3_CONTAINER=bsc-validator-3 NODE3_IP=10.102.0.13 NODE4_CONTAINER=bsc-rpc NODE4_IP=10.102.0.10"
}

chain_config_cosmos() {
    local cosmos_dir="${BENCH_REPO_ROOT}/bench-targets/chains/cosmos"
    CHAIN_RPC="http://localhost:8545"
    CHAIN_WS="ws://localhost:8546"
    CHAIN_CHAIN_ID=9000
    # bench-funder address 0xc6fe5d33615a1c52c08018c47e8bc53646a0e101 — pre-funded in genesis via init.sh
    CHAIN_KEYS="0x88cbead91aee890d27bf06e003ade3d4e952427e88f88d31d61d3ef5e5d54305"
    CHAIN_UP_CMD="make -C ${cosmos_dir} up"
    CHAIN_DOWN_CMD="make -C ${cosmos_dir} down"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:8545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="L1 (Evmos single-validator testnet)"
    # Single-node chain: geo topology simulation not applicable (no inter-node consensus latency)
    CHAIN_TOPOLOGY_ENV=""
}

chain_config_anvil() {
    CHAIN_RPC="http://localhost:18888"
    CHAIN_WS="ws://localhost:18888"
    CHAIN_CHAIN_ID=31337
    CHAIN_KEYS="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    CHAIN_UP_CMD="anvil --block-time 1 --accounts 1 --balance 1000000 --port 18888 --silent &"
    CHAIN_DOWN_CMD="pkill -f 'anvil.*18888' 2>/dev/null || true"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && sleep 1 && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:18888 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="Dev tool (no consensus, single node)"
}

chain_config_geth() {
    CHAIN_RPC="http://localhost:18889"
    CHAIN_WS="ws://localhost:18890"
    CHAIN_CHAIN_ID=1337
    CHAIN_KEYS="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    CHAIN_UP_CMD="docker run -d --name geth-dev --rm -p 18889:8545 -p 18890:8546 ethereum/client-go:latest --dev --http --http.addr 0.0.0.0 --http.api eth,net,web3,txpool,personal --ws --ws.addr 0.0.0.0 --ws.api eth,net,web3 --http.corsdomain '*' --dev.period 1"
    CHAIN_DOWN_CMD="docker stop geth-dev 2>/dev/null || true"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && sleep 2 && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:18889 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="Reference EVM (geth --dev, single node)"
}

chain_config_reth() {
    CHAIN_RPC="http://localhost:38545"
    CHAIN_WS="ws://localhost:38546"
    CHAIN_CHAIN_ID=1337
    CHAIN_KEYS="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    CHAIN_UP_CMD="docker run -d --name reth-dev --rm -p 38545:8545 -p 38546:8546 ghcr.io/paradigmxyz/reth:latest node --dev --dev.block-time 1s --http --http.addr 0.0.0.0 --http.api eth,net,web3,txpool --ws --ws.addr 0.0.0.0 --ws.api eth,net,web3 --http.corsdomain '*' --txpool.max-pending-txns 50000 --txpool.max-new-txns 50000 --txpool.max-account-slots 5000"
    CHAIN_DOWN_CMD="docker stop reth-dev 2>/dev/null || true"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && sleep 2 && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf http://localhost:38545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="Reference EVM (reth --dev, single node)"
}

chain_config_berachain() {
    local env_file="${BENCH_REPO_ROOT}/bench-targets/chains/berachain/.env"
    if [ -f "$env_file" ]; then
        # shellcheck source=/dev/null
        source "$env_file"
    fi
    CHAIN_RPC="${BERA_RPC:-http://localhost:8545}"
    CHAIN_WS="${BERA_WS:-ws://localhost:8546}"
    CHAIN_CHAIN_ID="${BERA_CHAIN_ID:-80087}"
    CHAIN_KEYS="0xfffdbb37105441e14b0ee6330d855d8504ff39e705c3afa8f859ac9865f99306"
    CHAIN_UP_CMD="cd ${BENCH_REPO_ROOT}/bench-targets/chains/berachain && make up"
    CHAIN_DOWN_CMD="cd ${BENCH_REPO_ROOT}/bench-targets/chains/berachain && make down"
    CHAIN_CLEAN_CMD="${CHAIN_DOWN_CMD} && ${CHAIN_UP_CMD}"
    CHAIN_STATUS_CMD="curl -sf \${CHAIN_RPC} -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
    CHAIN_TYPE="L1 (CometBFT, 4 validators, beacon-kit)"
}

# ── 5. Chain loader ──────────────────────────────────────────────────────────
load_chain_config() {
    local chain_name="${1:?Usage: load_chain_config <chain_name>}"
    local func="chain_config_${chain_name}"

    if ! declare -f "$func" > /dev/null 2>&1; then
        log_error "Unknown chain: '${chain_name}'"
        log_error "Available chains: ${_REGISTERED_CHAINS}"
        return 1
    fi

    # Reset variables before loading
    CHAIN_RPC="" CHAIN_WS="" CHAIN_CHAIN_ID="" CHAIN_KEYS=""
    CHAIN_UP_CMD="" CHAIN_DOWN_CMD="" CHAIN_CLEAN_CMD=""
    CHAIN_STATUS_CMD="" CHAIN_TYPE="" CHAIN_TOPOLOGY_ENV=""

    "$func"
}

list_chains() {
    echo "$_REGISTERED_CHAINS"
}

# ── 6. Results directory helpers ─────────────────────────────────────────────
timestamp() {
    date +%Y%m%d-%H%M%S
}

update_latest_symlink() {
    local dir="${1:?Usage: update_latest_symlink <dir>}"
    local parent
    parent="$(dirname "$dir")"
    local base
    base="$(basename "$dir")"
    ln -sfn "$base" "${parent}/latest"
}

make_run_dir() {
    local chain="${1:?Usage: make_run_dir <chain> <mode> <tag>}"
    local mode="${2:?}"
    local tag="${3:-}"
    local ts
    ts="$(timestamp)"
    local suffix="${ts}"
    [ -n "$tag" ] && suffix="${ts}_${tag}"
    local dir="${RESULTS_BASE}/runs/${chain}/${mode}/${suffix}"
    mkdir -p "$dir"
    update_latest_symlink "$dir"
    echo "$dir"
}

make_comparison_dir() {
    local chains_string="${1:?Usage: make_comparison_dir <chains_string> <mode>}"
    local mode="${2:?}"
    local ts
    ts="$(timestamp)"
    local dir="${RESULTS_BASE}/comparisons/${ts}_${chains_string}_${mode}"
    mkdir -p "$dir"
    update_latest_symlink "$dir"
    echo "$dir"
}

make_sweep_dir() {
    local param="${1:?Usage: make_sweep_dir <param>}"
    local ts
    ts="$(timestamp)"
    local dir="${RESULTS_BASE}/sweeps/${ts}_${param}"
    mkdir -p "$dir"
    update_latest_symlink "$dir"
    echo "$dir"
}

# ── 7. Index management ─────────────────────────────────────────────────────
index_add_run() {
    local path="${1:?}" chain="${2:?}" mode="${3:?}" env="${4:?}"
    local tps="${5:?}" p50="${6:?}" p99="${7:?}" confirmed_rate="${8:?}"

    local index_file="${RESULTS_BASE}/index.json"
    mkdir -p "$(dirname "$index_file")"

    if [ ! -f "$index_file" ]; then
        echo '[]' > "$index_file"
    fi

    python3 -c "
import json, sys, os
index_file = '$index_file'
with open(index_file) as f:
    data = json.load(f)
data.append({
    'path': '$path',
    'chain': '$chain',
    'mode': '$mode',
    'env': '$env',
    'tps': float('$tps'),
    'p50': float('$p50'),
    'p99': float('$p99'),
    'confirmed_rate': float('$confirmed_rate'),
    'timestamp': '$(timestamp)'
})
with open(index_file, 'w') as f:
    json.dump(data, f, indent=2)
" || log_warn "Failed to update index at ${index_file}"
}

# ── 8. Topology helpers ─────────────────────────────────────────────────────
apply_topology() {
    local layout="${1:?Usage: apply_topology <layout>}"
    if [ ! -x "$TOPOLOGY_SCRIPT" ]; then
        log_error "Topology script not found or not executable: ${TOPOLOGY_SCRIPT}"
        return 1
    fi
    bash "$TOPOLOGY_SCRIPT" apply "$layout"
}

clear_topology() {
    if [ ! -x "$TOPOLOGY_SCRIPT" ]; then
        log_error "Topology script not found or not executable: ${TOPOLOGY_SCRIPT}"
        return 1
    fi
    bash "$TOPOLOGY_SCRIPT" clear
}
