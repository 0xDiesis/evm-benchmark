#!/bin/bash
# Run the evm-benchmark against a Scroll l2geth standalone node.
#
# Usage:
#   ./run-bench.sh                          # default: 10000 tx burst
#   ./run-bench.sh --txs 4000 --execution sustained --tps 500 --duration 30
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# Scroll l2geth RPC endpoints (matches docker-compose.yml port mapping)
SCROLL_RPC="http://localhost:48545"
SCROLL_WS="ws://localhost:48546"
SCROLL_CHAIN_ID=53077

# Benchmark account private key (pre-funded in genesis)
# Bench 1: 0x11950BC14473845bb68c0a6C6B5c468854aedCBf  (sha256("scroll-bench-1"))
SCROLL_KEY="0xc0c85dc29d5c58039e502db807b6217cbb633ccd5f574d2449097e321abb89bc"
SCROLL_PREFUNDED_SENDERS=1

# Auto-detect chain ID from running node
DETECTED=$(curl -sf "${SCROLL_RPC}" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' 2>/dev/null | \
    python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "$SCROLL_CHAIN_ID")
SCROLL_CHAIN_ID="$DETECTED"

# Verify node is reachable
echo "Checking Scroll RPC endpoint..."
if curl -sf "$SCROLL_RPC" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' > /dev/null 2>&1; then
    echo "  $SCROLL_RPC: OK"
else
    echo "  $SCROLL_RPC: UNREACHABLE"
    echo ""
    echo "Start the Scroll l2geth node first:"
    echo "  cd bench-targets/chains/scroll && make up"
    exit 1
fi

echo ""
echo "Running bench against Scroll l2geth (chain_id=${SCROLL_CHAIN_ID})..."
echo ""

# Default args
DEFAULT_ARGS=(--execution burst --txs 10000 --batch-size 500 --wave-delay-ms 0 --senders "$SCROLL_PREFUNDED_SENDERS" --fund)

CLI_ARGS=("$@")
if [[ $# -eq 0 ]]; then
    CLI_ARGS=("${DEFAULT_ARGS[@]}")
fi

BENCH_KEY="$SCROLL_KEY" \
    cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$SCROLL_RPC" \
    --ws "$SCROLL_WS" \
    --chain-id "$SCROLL_CHAIN_ID" \
    --bench-name "scroll_l2geth" \
    --out "${SCRIPT_DIR}/report.json" \
    "${CLI_ARGS[@]}"

echo ""
echo "Report written to: bench-targets/chains/scroll/report.json"
