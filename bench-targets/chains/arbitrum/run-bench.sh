#!/bin/bash
# Run the evm-benchmark against an Arbitrum Nitro testnode (L2 sequencer).
#
# Usage:
#   ./run-bench.sh                          # default: 1000 tx burst
#   ./run-bench.sh --txs 4000 --execution sustained --tps 500 --duration 30
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# Arbitrum Nitro testnode L2 sequencer endpoints
ARB_RPC="http://127.0.0.1:8547"
ARB_WS="ws://127.0.0.1:8548"

# Nitro testnode L2 chain ID
ARB_CHAIN_ID=412346

# Pre-funded dev account from nitro-testnode
# This is the "l2owner" key, pre-funded with ETH on L2
ARB_KEY="0xb6b15c8cb491557369f3c7d2c287b053eb229daa9c22138887752191c9520659"
ARB_PREFUNDED_SENDERS=1

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
    ((requested_senders > ARB_PREFUNDED_SENDERS)); then
    echo "ERROR: Arbitrum testnode only pre-funds ${ARB_PREFUNDED_SENDERS} account." >&2
    echo "Reduce --senders to ${ARB_PREFUNDED_SENDERS} or less, or add --fund to provision more accounts first." >&2
    exit 1
fi

# Verify L2 sequencer is reachable
echo "Checking Arbitrum L2 sequencer RPC endpoint..."
if curl -sf "$ARB_RPC" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' > /dev/null 2>&1; then
    echo "  $ARB_RPC: OK"
else
    echo "  $ARB_RPC: UNREACHABLE"
    echo ""
    echo "Start the Arbitrum testnode first:"
    echo "  cd bench-targets/chains/arbitrum && make up"
    exit 1
fi

# Get chain ID from the sequencer to verify
CHAIN_ID_HEX=$(curl -sf "$ARB_RPC" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' | \
    python3 -c "import sys,json; print(json.load(sys.stdin)['result'])" 2>/dev/null || echo "0x0")
CHAIN_ID_DEC=$(python3 -c "print(int('${CHAIN_ID_HEX}', 16))" 2>/dev/null || echo "unknown")
echo ""
echo "Arbitrum L2 chain ID: ${CHAIN_ID_DEC} (${CHAIN_ID_HEX})"

# Override chain ID if the node reports something different from default
if [ "$CHAIN_ID_DEC" != "unknown" ] && [ "$CHAIN_ID_DEC" != "0" ]; then
    ARB_CHAIN_ID=$CHAIN_ID_DEC
fi

echo "Running evm-benchmark against Arbitrum Nitro L2 (chain_id=${ARB_CHAIN_ID})..."
echo ""

# Default args — single pre-funded key, use --fund to provision more senders
DEFAULT_ARGS=(--execution burst --txs 1000 --batch-size 100 --senders "$ARB_PREFUNDED_SENDERS" --fund)

CLI_ARGS=("$@")
if [[ $# -eq 0 ]]; then
    CLI_ARGS=("${DEFAULT_ARGS[@]}")
elif [[ "$has_sender_override" != "true" && "$has_funding" != "true" ]]; then
    CLI_ARGS+=(--senders "$ARB_PREFUNDED_SENDERS" --fund)
fi

BENCH_KEY="$ARB_KEY" \
    cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$ARB_RPC" \
    --ws "$ARB_WS" \
    --chain-id "$ARB_CHAIN_ID" \
    --bench-name "arbitrum_nitro" \
    --out "${SCRIPT_DIR}/report.json" \
    "${CLI_ARGS[@]}"

echo ""
echo "Report written to: bench-targets/chains/arbitrum/report.json"
