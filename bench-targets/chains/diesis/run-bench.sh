#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_REPO_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
DIESIS_REPO_DIR="${DIESIS_REPO_DIR:-${BENCH_REPO_DIR}/../diesis}"
HARNESS_MANIFEST="${BENCH_REPO_DIR}/crates/evm-benchmark/Cargo.toml"

DIESIS_RPC="${DIESIS_RPC:-http://localhost:8545,http://localhost:8555,http://localhost:8565,http://localhost:8575}"
DIESIS_WS="${DIESIS_WS:-ws://localhost:8546}"
DIESIS_CHAIN_ID="${DIESIS_CHAIN_ID:-19803}"
BENCH_NAME="${BENCH_NAME:-diesis_local}"
BENCH_OUT="${BENCH_OUT:-${SCRIPT_DIR}/report.json}"
BENCH_KEY_DEFAULT="0x0000000000000000000000000000000000000000000000000000000000000001,0x0000000000000000000000000000000000000000000000000000000000000002,0x0000000000000000000000000000000000000000000000000000000000000003,0x0000000000000000000000000000000000000000000000000000000000000004"
DIESIS_PREFUNDED_SENDERS=4

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
    ((requested_senders > DIESIS_PREFUNDED_SENDERS)); then
    echo "ERROR: Diesis only pre-funds ${DIESIS_PREFUNDED_SENDERS} validator accounts by default." >&2
    echo "Reduce --senders to ${DIESIS_PREFUNDED_SENDERS} or less, or add --fund to provision more accounts first." >&2
    exit 1
fi

if ! curl -sf http://localhost:8545 \
    -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' >/dev/null 2>&1; then
    echo "Diesis node is not reachable on :8545"
    echo "Start it with: make -C \"${DIESIS_REPO_DIR}\" e2e-up-release"
    exit 1
fi

# Default to the 4 prefunded validator accounts unless callers opt into `--fund`
# or explicitly choose a smaller sender count.
CLI_ARGS=("$@")
if [[ $# -eq 0 ]]; then
    CLI_ARGS=(--execution burst --txs 1000 --batch-size 100 --senders "$DIESIS_PREFUNDED_SENDERS")
elif [[ "$has_sender_override" != "true" && "$has_funding" != "true" ]]; then
    CLI_ARGS+=(--senders "$DIESIS_PREFUNDED_SENDERS")
fi

BENCH_KEY="${BENCH_KEY:-${BENCH_KEY_DEFAULT}}" \
    cargo run --release --manifest-path "${HARNESS_MANIFEST}" -- \
    --rpc-endpoints "${DIESIS_RPC}" \
    --ws "${DIESIS_WS}" \
    --chain-id "${DIESIS_CHAIN_ID}" \
    --bench-name "${BENCH_NAME}" \
    --out "${BENCH_OUT}" \
    "${CLI_ARGS[@]}"
