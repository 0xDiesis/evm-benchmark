#!/bin/bash
# Run the evm-benchmark against an Optimism L2 via Supersim.
#
# Usage:
#   ./run-bench.sh                          # default: 1000 tx burst
#   ./run-bench.sh --txs 4000 --execution sustained --tps 500 --duration 30
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# Supersim L2 RPC endpoints
OP_RPC="http://127.0.0.1:9545"
OP_WS="ws://127.0.0.1:9546"

# Supersim OP Stack L2 chain ID
OP_CHAIN_ID=901

# Hardhat account #0 — pre-funded in Supersim with 10000 ETH
OP_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
OP_PREFUNDED_SENDERS=1

has_funding=false
has_sender_override=false
requested_senders=""

for ((i = 1; i <= $#; i++)); do
    arg="${!i}"
    case "$arg" in
        --fund)
            has_funding=true
            ;;
        --senders)
            next_index=$((i + 1))
            if ((next_index > $#)); then
                echo "ERROR: --senders requires a value" >&2
                exit 1
            fi
            has_sender_override=true
            requested_senders="${!next_index}"
            ;;
    esac
done

if [[ "$has_sender_override" == "true" && "$has_funding" != "true" ]] &&
    [[ "$requested_senders" =~ ^[0-9]+$ ]] &&
    ((requested_senders > OP_PREFUNDED_SENDERS)); then
    echo "ERROR: Supersim only pre-funds ${OP_PREFUNDED_SENDERS} account (Hardhat #0)." >&2
    echo "Reduce --senders to ${OP_PREFUNDED_SENDERS} or add --fund to provision more accounts first." >&2
    exit 1
fi

# Verify L2 is reachable
echo "Checking Optimism L2 RPC endpoint..."
if curl -sf "$OP_RPC" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' > /dev/null 2>&1; then
    echo "  $OP_RPC: OK"
else
    echo "  $OP_RPC: UNREACHABLE"
    echo ""
    echo "Start Supersim first:"
    echo "  cd bench-targets/chains/optimism && make up"
    exit 1
fi

# Get chain ID from node to verify
CHAIN_ID_HEX=$(curl -sf "$OP_RPC" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' | \
    python3 -c "import sys,json; print(json.load(sys.stdin)['result'])" 2>/dev/null || echo "0x0")
CHAIN_ID_DEC=$(python3 -c "print(int('${CHAIN_ID_HEX}', 16))" 2>/dev/null || echo "unknown")
echo ""
echo "Optimism L2 chain ID: ${CHAIN_ID_DEC} (${CHAIN_ID_HEX})"

# Override chain ID if the node reports something different from default
if [ "$CHAIN_ID_DEC" != "unknown" ] && [ "$CHAIN_ID_DEC" != "0" ]; then
    OP_CHAIN_ID=$CHAIN_ID_DEC
fi

echo "Running evm-benchmark against Optimism L2 via Supersim (chain_id=${OP_CHAIN_ID})..."
echo ""

# Default args — single pre-funded account unless --fund is used to provision more
DEFAULT_ARGS=(--execution burst --txs 1000 --batch-size 100 --senders "$OP_PREFUNDED_SENDERS")

CLI_ARGS=("$@")
if [[ $# -eq 0 ]]; then
    CLI_ARGS=("${DEFAULT_ARGS[@]}")
elif [[ "$has_sender_override" != "true" && "$has_funding" != "true" ]]; then
    CLI_ARGS+=(--senders "$OP_PREFUNDED_SENDERS")
fi

BENCH_KEY="$OP_KEY" \
    cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$OP_RPC" \
    --ws "$OP_WS" \
    --chain-id "$OP_CHAIN_ID" \
    --bench-name "optimism_supersim" \
    --fund \
    --out "${SCRIPT_DIR}/report.json" \
    "${CLI_ARGS[@]}"

echo ""
echo "Report written to: bench-targets/chains/optimism/report.json"
