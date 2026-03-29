#!/bin/bash
# Run the evm-benchmark against an Evmos single-node localnet (v20.0.0).
#
# Usage:
#   ./run-bench.sh                          # default: 2000 tx burst
#   ./run-bench.sh --txs 4000 --execution sustained --tps 200 --duration 30
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

EVMOS_RPC="http://localhost:8545"
EVMOS_WS="ws://localhost:8546"
EVMOS_CHAIN_ID=9000

# Verify node is reachable
if ! curl -sf "$EVMOS_RPC" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' > /dev/null 2>&1; then
    echo "ERROR: Evmos RPC not responding at ${EVMOS_RPC}" >&2
    echo "Start the node first:  make up" >&2
    exit 1
fi

# Auto-detect chain ID from running node
DETECTED=$(curl -sf "$EVMOS_RPC" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' 2>/dev/null | \
    python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "$EVMOS_CHAIN_ID")
EVMOS_CHAIN_ID="$DETECTED"

# bench-funder: 0xc6fe5d33615a1c52c08018c47e8bc53646a0e101 (pre-funded in genesis)
export BENCH_KEY="${BENCH_KEY:-0x88cbead91aee890d27bf06e003ade3d4e952427e88f88d31d61d3ef5e5d54305}"

echo "Running bench against Evmos localnet (chain_id=${EVMOS_CHAIN_ID})..."
echo ""

DEFAULT_ARGS=(--execution burst --txs 2000 --batch-size 200 --wave-delay-ms 0 --senders 200 --fund)

CLI_ARGS=("$@")
if [[ $# -eq 0 ]]; then
    CLI_ARGS=("${DEFAULT_ARGS[@]}")
fi

cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$EVMOS_RPC" \
    --ws "$EVMOS_WS" \
    --chain-id "$EVMOS_CHAIN_ID" \
    --bench-name "evmos_localnet" \
    --out "${SCRIPT_DIR}/report.json" \
    "${CLI_ARGS[@]}"

echo ""
echo "Report written to: bench-targets/chains/cosmos/report.json"
