#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# Discover ports if .env doesn't exist yet
if [ ! -f "${SCRIPT_DIR}/.env" ]; then
    bash "${SCRIPT_DIR}/discover-ports.sh"
fi

# shellcheck source=/dev/null
source "${SCRIPT_DIR}/.env"

# Pre-funded Kurtosis devnet accounts (from beacon-kit/kurtosis/src/constants.star)
# Primary account has ~10^30 wei, others have ~86k ETH each
BERA_KEYS="0xfffdbb37105441e14b0ee6330d855d8504ff39e705c3afa8f859ac9865f99306,0x9b9bc88a144fff869ae2f4ea8e252f2494d9b52ea1008d0b3537dad27ab489d5,0x23b19fd0ba67f921bc1f5a133bfe452060d129f025fcf1be75c6964551b1208a,0x0e67856b2a42ca52862a60d11e3ac57871988aefe7a28ecd20bd8c2dec55da25"

echo "Running evm-benchmark against Berachain Kurtosis devnet..."
echo "  RPC:      ${BERA_RPC}"
echo "  WS:       ${BERA_WS}"
echo "  Chain ID: ${BERA_CHAIN_ID}"

BENCH_KEY="$BERA_KEYS" cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$BERA_RPC" \
    --ws "$BERA_WS" \
    --chain-id "$BERA_CHAIN_ID" \
    --bench-name "berachain_kurtosis" \
    --senders 200 \
    --fund \
    --out "${SCRIPT_DIR}/report.json" \
    ${@:---execution burst --txs 5000 --batch-size 200 --wave-delay-ms 0}
