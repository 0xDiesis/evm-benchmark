#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# Sei EVM RPC endpoints (mock_balances auto-funds any account)
SEI_RPC="http://localhost:28545,http://localhost:28547,http://localhost:28549,http://localhost:28551"
SEI_WS="ws://localhost:28546"
SEI_CHAIN_ID=713714

echo "Running evm-benchmark against Sei localnet..."
cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$SEI_RPC" \
    --ws "$SEI_WS" \
    --chain-id "$SEI_CHAIN_ID" \
    --bench-name "sei_localnet" \
    --senders 200 \
    --out "${SCRIPT_DIR}/report.json" \
    ${@:---execution burst --txs 10000 --batch-size 500 --wave-delay-ms 0}
