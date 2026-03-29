#!/bin/bash
# Run the evm-benchmark against a BSC local cluster.
#
# Usage:
#   ./run-bench.sh                          # default: 10000 tx burst
#   ./run-bench.sh --txs 4000 --execution sustained --tps 500 --duration 30
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# BSC cluster RPC endpoints (matches docker-compose.yml port mapping)
BSC_RPC="http://localhost:8545,http://localhost:8645,http://localhost:8745,http://localhost:8845"
BSC_WS="ws://localhost:8546"
BSC_CHAIN_ID=714714

# Benchmark account private keys (pre-funded in genesis with 100k BNB each)
# These are the 4 benchmark keys derived from keccak("bsc-bench-N"):
#   Bench 1: 0x528562E4EA1DFE07B63a6dfC20f8048a9c2E49AB
#   Bench 2: 0x8996D198ae008C81b52Ca95DF26BDabE8cE02684
#   Bench 3: 0x953A381425358C1Abd81D95f9548f242014C7dd4
#   Bench 4: 0x13e77aB15Febd5e3D7dE08e9b402518C436dC69e
BSC_KEY="0xb5a03afd7e912d137a7ec5e824c0aacba543a455c618acdf46843b9890087bca,0x6e36ad7e68ae0b565970cc55c8fa4c69ba753e9a9210d80b8030b6db6eceb667,0x5177497e2c518042c97b07ef08839f60b88bf3437ad93c83f4dfe66b1fd06014,0x0b48828bc5954f830b5ed3dcaed08ffad1f76a27735d877fa262fe26a92a353a"
BSC_PREFUNDED_SENDERS=4

# Auto-detect chain ID from running node
DETECTED=$(curl -sf "http://localhost:8545" -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' 2>/dev/null | \
    python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "$BSC_CHAIN_ID")
BSC_CHAIN_ID="$DETECTED"

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
    ((requested_senders > BSC_PREFUNDED_SENDERS)); then
    echo "ERROR: BSC only pre-funds ${BSC_PREFUNDED_SENDERS} benchmark accounts." >&2
    echo "Reduce --senders to ${BSC_PREFUNDED_SENDERS} or less, or add --fund to provision more accounts first." >&2
    exit 1
fi

# Verify nodes are reachable
echo "Checking BSC RPC endpoints..."
for url in ${BSC_RPC//,/ }; do
    if curl -sf "$url" -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' > /dev/null 2>&1; then
        echo "  $url: OK"
    else
        echo "  $url: UNREACHABLE"
        echo ""
        echo "Start the BSC cluster first:"
        echo "  cd bench-targets/chains/bsc && make up"
        exit 1
    fi
done

echo ""
echo "Running bench against BSC (chain_id=${BSC_CHAIN_ID})..."
echo ""

# Default args
DEFAULT_ARGS=(--execution burst --txs 10000 --batch-size 500 --wave-delay-ms 0 --senders "$BSC_PREFUNDED_SENDERS" --fund)

CLI_ARGS=("$@")
if [[ $# -eq 0 ]]; then
    CLI_ARGS=("${DEFAULT_ARGS[@]}")
elif [[ "$has_sender_override" != "true" && "$has_funding" != "true" ]]; then
    CLI_ARGS+=(--senders "$BSC_PREFUNDED_SENDERS" --fund)
fi

BENCH_KEY="$BSC_KEY" \
    cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$BSC_RPC" \
    --ws "$BSC_WS" \
    --chain-id "$BSC_CHAIN_ID" \
    --bench-name "bsc_cluster" \
    --out "${SCRIPT_DIR}/report.json" \
    "${CLI_ARGS[@]}"

echo ""
echo "Report written to: bench-targets/chains/bsc/report.json"
