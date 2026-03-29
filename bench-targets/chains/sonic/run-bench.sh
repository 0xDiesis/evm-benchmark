#!/bin/bash
# Run the evm-benchmark against a Sonic fakenet.
#
# Usage:
#   ./run-bench.sh                          # default: 1000 tx burst
#   ./run-bench.sh --txs 4000 --execution sustained --tps 500 --duration 30
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# Sonic fakenet RPC endpoints (matches docker-compose.yml port mapping)
SONIC_RPC="http://localhost:18545,http://localhost:18645,http://localhost:18745,http://localhost:18845"
SONIC_WS="ws://localhost:18546"

# Sonic fakenet chain ID (from sonic example-genesis.json NetworkID)
SONIC_CHAIN_ID=4003

# Sonic fakenet validator private keys (from evmcore/apply_fake_genesis.go)
# All 4 accounts are pre-funded with ~10^9 S tokens in the fakenet genesis.
# Using all 4 keys spreads txs across accounts for higher throughput.
SONIC_KEY="0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7,0x3144c0aa4ced56dc15c79b045bc5559a5ac9363d98db6df321fe3847a103740f,0x04a531f967898df5dbe223b67989b248e23c1c356a3f6717775cccb7fe53482c,0x00ca81d4fe11c23fae8b5e5b06f9fe952c99ca46abaec8bda70a678cd0314dde"
SONIC_PREFUNDED_SENDERS=4

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
    ((requested_senders > SONIC_PREFUNDED_SENDERS)); then
    echo "ERROR: Sonic only pre-funds ${SONIC_PREFUNDED_SENDERS} validator accounts." >&2
    echo "Reduce --senders to ${SONIC_PREFUNDED_SENDERS} or less, or add --fund to provision more accounts first." >&2
    exit 1
fi

# Verify nodes are reachable
echo "Checking Sonic RPC endpoints..."
for url in ${SONIC_RPC//,/ }; do
    if curl -sf "$url" -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' > /dev/null 2>&1; then
        echo "  $url: OK"
    else
        echo "  $url: UNREACHABLE"
        echo ""
        echo "Start the Sonic testnet first:"
        echo "  cd bench-targets/sonic && docker compose up --build -d && ./connect-peers.sh"
        exit 1
    fi
done

# Get chain ID from first node to verify
CHAIN_ID_HEX=$(curl -sf "http://localhost:18545" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' | \
    python3 -c "import sys,json; print(json.load(sys.stdin)['result'])" 2>/dev/null || echo "0x0")
CHAIN_ID_DEC=$(python3 -c "print(int('${CHAIN_ID_HEX}', 16))" 2>/dev/null || echo "unknown")
echo ""
echo "Sonic chain ID: ${CHAIN_ID_DEC} (${CHAIN_ID_HEX})"

# Override chain ID if the node reports something different from default
if [ "$CHAIN_ID_DEC" != "unknown" ] && [ "$CHAIN_ID_DEC" != "0" ]; then
    SONIC_CHAIN_ID=$CHAIN_ID_DEC
fi

echo "Running evm-benchmark against Sonic fakenet (chain_id=${SONIC_CHAIN_ID})..."
echo ""

# Default args if none provided. Sonic only has 4 pre-funded validator keys unless
# callers opt into `--fund` to provision more senders first.
DEFAULT_ARGS=(--execution burst --txs 1000 --batch-size 100 --senders "$SONIC_PREFUNDED_SENDERS")

CLI_ARGS=("$@")
if [[ $# -eq 0 ]]; then
    CLI_ARGS=("${DEFAULT_ARGS[@]}")
elif [[ "$has_sender_override" != "true" && "$has_funding" != "true" ]]; then
    CLI_ARGS+=(--senders "$SONIC_PREFUNDED_SENDERS")
fi

BENCH_KEY="$SONIC_KEY" \
    cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$SONIC_RPC" \
    --ws "$SONIC_WS" \
    --chain-id "$SONIC_CHAIN_ID" \
    --bench-name "sonic_fakenet" \
    --out "${SCRIPT_DIR}/report.json" \
    "${CLI_ARGS[@]}"

echo ""
echo "Report written to: bench-targets/sonic/report.json"
