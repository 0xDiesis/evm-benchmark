#!/bin/bash
# Comprehensive EVM Benchmark Test Suite
# Runs each chain through identical test scenarios for fair comparison.
#
# Usage:
#   ./bench-targets/test-suite.sh              # run all chains
#   ./bench-targets/test-suite.sh diesis sonic  # run specific chains only
#
# Prerequisites:
#   Each chain must be started independently before running its tests.
#   The suite stops/starts chains to ensure isolated resource usage.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIESIS_REPO_ROOT="${DIESIS_REPO_ROOT:-${BENCH_REPO_ROOT}/../diesis}"

# Diesis chain tests require the Diesis source repo. Skip them if not present.
if [[ ! -d "${DIESIS_REPO_ROOT}" ]]; then
    echo "WARNING: Diesis source repo not found at ${DIESIS_REPO_ROOT}; diesis-chain tests will fail." >&2
    echo "         Set DIESIS_REPO_ROOT to a Diesis checkout, or run only non-diesis chain tests." >&2
fi

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RUN_DIR="${SCRIPT_DIR}/results/suite_${TIMESTAMP}"
mkdir -p "$RUN_DIR"

HARNESS="${BENCH_REPO_ROOT}/target/release/evm-benchmark"

# ── Chain configurations ────────────────────────────────────────────
declare -A CHAIN_RPC CHAIN_WS CHAIN_ID CHAIN_KEY CHAIN_START CHAIN_STOP CHAIN_TYPE

# Diesis
CHAIN_RPC[diesis]="http://localhost:8545,http://localhost:8555,http://localhost:8565,http://localhost:8575"
CHAIN_WS[diesis]="ws://localhost:8546"
CHAIN_ID[diesis]=19803
CHAIN_KEY[diesis]="0x0000000000000000000000000000000000000000000000000000000000000001"
CHAIN_START[diesis]="cd ${DIESIS_REPO_ROOT} && make e2e-up-release"
CHAIN_STOP[diesis]="cd ${DIESIS_REPO_ROOT} && make e2e-down"
CHAIN_TYPE[diesis]="L1 (Mysticeti BFT, 4 validators)"

# Sonic
CHAIN_RPC[sonic]="http://localhost:18545,http://localhost:18645,http://localhost:18745,http://localhost:18845"
CHAIN_WS[sonic]="ws://localhost:18546"
CHAIN_ID[sonic]=4003
CHAIN_KEY[sonic]="0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7"
CHAIN_START[sonic]="cd ${SCRIPT_DIR}/chains/sonic && make up"
CHAIN_STOP[sonic]="cd ${SCRIPT_DIR}/chains/sonic && make down"
CHAIN_TYPE[sonic]="L1 (Lachesis aBFT, 4 validators)"

# Sei
CHAIN_RPC[sei]="http://localhost:28545,http://localhost:28547,http://localhost:28549,http://localhost:28551"
CHAIN_WS[sei]="ws://localhost:28546"
CHAIN_ID[sei]=713714
CHAIN_KEY[sei]=""  # mock_balances auto-funds
CHAIN_START[sei]="cd ${SCRIPT_DIR}/chains/sei && make up"
CHAIN_STOP[sei]="cd ${SCRIPT_DIR}/chains/sei && make down"
CHAIN_TYPE[sei]="L1 (sei-tendermint BFT, 4 validators)"

# Avalanche
CHAIN_RPC[avalanche]="http://localhost:9650/ext/bc/C/rpc"
CHAIN_WS[avalanche]="ws://localhost:9650/ext/bc/C/ws"
CHAIN_ID[avalanche]=43112
CHAIN_KEY[avalanche]="0x56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027"
CHAIN_START[avalanche]="cd ${SCRIPT_DIR}/chains/avalanche && make up"
CHAIN_STOP[avalanche]="cd ${SCRIPT_DIR}/chains/avalanche && make down"
CHAIN_TYPE[avalanche]="L1 (Snowman consensus, 5 nodes)"

# Anvil (baseline - no consensus)
CHAIN_RPC[anvil]="http://localhost:18888"
CHAIN_WS[anvil]="ws://localhost:18888"
CHAIN_ID[anvil]=31337
CHAIN_KEY[anvil]="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
CHAIN_START[anvil]="anvil --block-time 1 --accounts 1 --balance 1000000 --port 18888 --silent &"
CHAIN_STOP[anvil]="pkill -f 'anvil.*18888' 2>/dev/null || true"
CHAIN_TYPE[anvil]="Dev tool (no consensus, single node)"

# Geth dev (reference EVM)
CHAIN_RPC[geth]="http://localhost:18889"
CHAIN_WS[geth]="ws://localhost:18890"
CHAIN_ID[geth]=1337
CHAIN_KEY[geth]="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
CHAIN_START[geth]="docker run -d --name geth-dev --rm -p 18889:8545 -p 18890:8546 ethereum/client-go:latest --dev --http --http.addr 0.0.0.0 --http.api eth,net,web3,txpool,personal --ws --ws.addr 0.0.0.0 --ws.api eth,net,web3 --http.corsdomain '*' --dev.period 1 && sleep 3 && DEV_ACCT=\$(curl -sf http://localhost:18889 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_accounts\",\"params\":[],\"id\":1}' | python3 -c 'import sys,json; print(json.load(sys.stdin)[\"result\"][0])') && curl -sf http://localhost:18889 -X POST -H 'Content-Type: application/json' -d \"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"method\\\":\\\"eth_sendTransaction\\\",\\\"params\\\":[{\\\"from\\\":\\\"$DEV_ACCT\\\",\\\"to\\\":\\\"0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266\\\",\\\"value\\\":\\\"0x21E19E0C9BAB2400000\\\"}],\\\"id\\\":1}\" > /dev/null && sleep 2"
CHAIN_STOP[geth]="docker stop geth-dev 2>/dev/null || true"
CHAIN_TYPE[geth]="Reference EVM (geth --dev, single node)"

# Reth dev (reference EVM — same execution engine as Diesis and Berachain)
CHAIN_RPC[reth]="http://localhost:38545"
CHAIN_WS[reth]="ws://localhost:38546"
CHAIN_ID[reth]=1337
CHAIN_KEY[reth]="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
CHAIN_START[reth]="docker run -d --name reth-dev --rm -p 38545:8545 -p 38546:8546 ghcr.io/paradigmxyz/reth:latest node --dev --dev.block-time 1s --http --http.addr 0.0.0.0 --http.api eth,net,web3,txpool --ws --ws.addr 0.0.0.0 --ws.api eth,net,web3 --http.corsdomain '*' --txpool.max-pending-txns 50000 --txpool.max-new-txns 50000 --txpool.max-account-slots 5000 && sleep 5"
CHAIN_STOP[reth]="docker stop reth-dev 2>/dev/null || true"
CHAIN_TYPE[reth]="Reference EVM (reth --dev, single node)"

# Berachain (Kurtosis — dynamic ports, discover before use)
if [ -f "${SCRIPT_DIR}/chains/berachain/.env" ]; then
    source "${SCRIPT_DIR}/chains/berachain/.env"
fi
CHAIN_RPC[berachain]="${BERA_RPC:-http://localhost:8545}"
CHAIN_WS[berachain]="${BERA_WS:-ws://localhost:8546}"
CHAIN_ID[berachain]="${BERA_CHAIN_ID:-80087}"
CHAIN_KEY[berachain]="0xfffdbb37105441e14b0ee6330d855d8504ff39e705c3afa8f859ac9865f99306"
CHAIN_START[berachain]="cd ${SCRIPT_DIR}/chains/berachain && make up"
CHAIN_STOP[berachain]="cd ${SCRIPT_DIR}/chains/berachain && make down"
CHAIN_TYPE[berachain]="L1 (CometBFT, 4 validators, beacon-kit)"

# ── Test scenarios ──────────────────────────────────────────────────
# Each test is: name, tx_count, senders, batch_size, wave_delay, extra_args
TESTS=(
    # Burst tests at increasing load
    "burst_1k:1000:200:200:0:"
    "burst_5k:5000:200:500:0:"
    "burst_10k:10000:200:500:0:"
    "burst_20k:20000:200:500:0:"

    # Sustained load tests
    "sustained_100tps:0:200:100:0:--execution sustained --tps 100 --duration 30"
    "sustained_500tps:0:200:100:0:--execution sustained --tps 500 --duration 30"
)

# ── Helper functions ────────────────────────────────────────────────
log() { echo "[$(date +%H:%M:%S)] $*"; }

run_test() {
    local CHAIN="$1"
    local TEST_SPEC="$2"
    IFS=':' read -r TEST_NAME TX_COUNT SENDERS BATCH WAVE_DELAY EXTRA <<< "$TEST_SPEC"

    local OUT="${RUN_DIR}/${CHAIN}_${TEST_NAME}.json"

    # Build base args
    local ARGS="--rpc-endpoints ${CHAIN_RPC[$CHAIN]}"
    ARGS="$ARGS --ws ${CHAIN_WS[$CHAIN]}"
    ARGS="$ARGS --chain-id ${CHAIN_ID[$CHAIN]}"
    ARGS="$ARGS --bench-name ${CHAIN}_${TEST_NAME}"
    ARGS="$ARGS --senders $SENDERS --fund"
    ARGS="$ARGS --out $OUT"
    ARGS="$ARGS --quiet"

    if [ -n "$EXTRA" ]; then
        ARGS="$ARGS $EXTRA"
    else
        ARGS="$ARGS --execution burst --txs $TX_COUNT --batch-size $BATCH --wave-delay-ms $WAVE_DELAY"
    fi

    # Set key
    if [ -n "${CHAIN_KEY[$CHAIN]}" ]; then
        export BENCH_KEY="${CHAIN_KEY[$CHAIN]}"
    else
        unset BENCH_KEY 2>/dev/null || true
    fi

    log "  Running $TEST_NAME on $CHAIN..."
    if $HARNESS $ARGS 2>&1 | tail -1; then
        # Extract key metrics
        if [ -f "$OUT" ]; then
            python3 -c "
import json
with open('$OUT') as f:
    r = json.load(f)['results']
    print(f'    -> {r[\"confirmed\"]}/{r[\"submitted\"]} confirmed, {r[\"confirmed_tps\"]:.0f} TPS, p50={r[\"latency\"][\"p50\"]}ms')
" 2>/dev/null || true
        fi
    else
        log "    -> FAILED"
    fi
}

check_rpc() {
    local URL="$1"
    curl -sf "$URL" -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' > /dev/null 2>&1
}

# ── Main ────────────────────────────────────────────────────────────
CHAINS_TO_TEST=("${@:-diesis sonic sei avalanche berachain reth anvil geth}")

echo "╔══════════════════════════════════════════════════════╗"
echo "║     EVM Benchmark Test Suite — ${TIMESTAMP}        ║"
echo "╚══════════════════════════════════════════════════════╝"
echo ""
echo "Chains: ${CHAINS_TO_TEST[*]}"
echo "Tests:  ${#TESTS[@]} scenarios per chain"
echo "Output: ${RUN_DIR}/"
echo ""

# Build harness
log "Building evm-benchmark (release)..."
cargo build -p evm-benchmark --release --manifest-path "${BENCH_REPO_ROOT}/Cargo.toml" 2>&1 | tail -1

for CHAIN in "${CHAINS_TO_TEST[@]}"; do
    if [ -z "${CHAIN_RPC[$CHAIN]+x}" ]; then
        log "Unknown chain: $CHAIN (skipping)"
        continue
    fi

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    log "$CHAIN — ${CHAIN_TYPE[$CHAIN]}"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    # Check if chain is running
    FIRST_RPC=$(echo "${CHAIN_RPC[$CHAIN]}" | cut -d',' -f1)
    if ! check_rpc "$FIRST_RPC"; then
        log "  Chain not running. Skipping."
        log "  Start with: ${CHAIN_START[$CHAIN]}"
        continue
    fi

    # Run all test scenarios
    for TEST in "${TESTS[@]}"; do
        run_test "$CHAIN" "$TEST"
    done
done

# ── Generate summary ────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
log "Generating summary..."

python3 << PYEOF
import json, os, glob

run_dir = "$RUN_DIR"
results = []

for f in sorted(glob.glob(os.path.join(run_dir, "*.json"))):
    with open(f) as fh:
        d = json.load(fh)
        r = d['results']
        name = d['benchmark']
        chain = name.split('_')[0]
        test = '_'.join(name.split('_')[1:])
        results.append({
            'chain': chain, 'test': test,
            'submitted': r['submitted'], 'confirmed': r['confirmed'],
            'tps': r['confirmed_tps'], 'p50': r['latency']['p50'],
            'p99': r['latency']['p99']
        })

if not results:
    print("No results found!")
    exit(0)

# Print summary table
print(f"\n{'Chain':<12} {'Test':<16} {'TPS':>8} {'p50':>8} {'p99':>8} {'Confirmed':>12}")
print("-" * 72)
for r in results:
    rate = f"{r['confirmed']}/{r['submitted']}"
    print(f"{r['chain']:<12} {r['test']:<16} {r['tps']:>8.0f} {r['p50']:>7}ms {r['p99']:>7}ms {rate:>12}")

# Save summary CSV
csv_path = os.path.join(run_dir, "summary.csv")
with open(csv_path, 'w') as f:
    f.write("chain,test,submitted,confirmed,tps,p50_ms,p99_ms\n")
    for r in results:
        f.write(f"{r['chain']},{r['test']},{r['submitted']},{r['confirmed']},{r['tps']:.1f},{r['p50']},{r['p99']}\n")
print(f"\nCSV saved to: {csv_path}")
PYEOF

echo ""
log "Test suite complete. Results: ${RUN_DIR}/"
