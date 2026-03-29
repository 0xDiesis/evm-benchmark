#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# Avalanche C-Chain RPC (path includes /ext/bc/C/rpc)
AVAX_RPC="http://localhost:9650/ext/bc/C/rpc,http://localhost:9660/ext/bc/C/rpc,http://localhost:9670/ext/bc/C/rpc,http://localhost:9680/ext/bc/C/rpc,http://localhost:9690/ext/bc/C/rpc"
AVAX_WS="ws://localhost:9650/ext/bc/C/ws"
AVAX_CHAIN_ID=43112

# Pre-funded C-Chain account
AVAX_KEY="0x56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027"

echo "Running evm-benchmark against Avalanche C-Chain..."
BENCH_KEY="$AVAX_KEY" cargo run -p evm-benchmark --release --manifest-path "${REPO_ROOT}/Cargo.toml" -- \
    --rpc-endpoints "$AVAX_RPC" \
    --ws "$AVAX_WS" \
    --chain-id "$AVAX_CHAIN_ID" \
    --bench-name "avalanche_cchain" \
    --senders 200 --fund \
    --out "${SCRIPT_DIR}/report.json" \
    ${@:---execution burst --txs 10000 --batch-size 500 --wave-delay-ms 0}
