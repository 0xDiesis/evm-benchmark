#!/bin/bash
# Run identical benchmarks on both Diesis and Sonic, then generate comparison report.
#
# Prerequisites:
#   - Diesis e2e cluster running: make e2e-up-release (from repo root)
#   - Sonic fakenet running: cd bench-targets/sonic && make up
#
# Usage:
#   ./bench-targets/run-comparison.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIESIS_REPO_ROOT="${DIESIS_REPO_ROOT:-${BENCH_REPO_ROOT}/../diesis}"
if [[ ! -d "${DIESIS_REPO_ROOT}" ]]; then
    echo "ERROR: this script requires the Diesis source repo at \${DIESIS_REPO_ROOT}." >&2
    echo "       Not found at: ${DIESIS_REPO_ROOT}" >&2
    echo "       Set DIESIS_REPO_ROOT to a Diesis checkout and re-run." >&2
    exit 2
fi
RESULTS_DIR="${SCRIPT_DIR}/results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RUN_DIR="${RESULTS_DIR}/${TIMESTAMP}"

mkdir -p "${RUN_DIR}"

# ── Shared bench parameters ─────────────────────────────────────────────────
# These are identical for both chains so results are directly comparable.
BURST_TXS=2000
SUSTAINED_TPS=200
SUSTAINED_DURATION=30
BATCH_SIZE=200
NUM_SENDERS=200

# ── Diesis config ───────────────────────────────────────────────────────────
# Ports: node-1=8545, node-2=8555, node-3=8565, node-4=8575 (from docker-compose.e2e.yml)
DIESIS_RPC="http://localhost:8545,http://localhost:8555,http://localhost:8565,http://localhost:8575"
DIESIS_WS="ws://localhost:8546"
DIESIS_CHAIN_ID=19803
# 4 deterministic validator keys (keys 1-4)
DIESIS_KEYS="0x0000000000000000000000000000000000000000000000000000000000000001,0x0000000000000000000000000000000000000000000000000000000000000002,0x0000000000000000000000000000000000000000000000000000000000000003,0x0000000000000000000000000000000000000000000000000000000000000004"
DIESIS_KEY_SINGLE="0x0000000000000000000000000000000000000000000000000000000000000001"

# ── Sonic config ────────────────────────────────────────────────────────────
SONIC_RPC="http://localhost:18545,http://localhost:18645,http://localhost:18745,http://localhost:18845"
SONIC_WS="ws://localhost:18546"
SONIC_CHAIN_ID=4003
# 4 fakenet validator keys (from evmcore/apply_fake_genesis.go)
SONIC_KEYS="0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7,0x3144c0aa4ced56dc15c79b045bc5559a5ac9363d98db6df321fe3847a103740f,0x04a531f967898df5dbe223b67989b248e23c1c356a3f6717775cccb7fe53482c,0x00ca81d4fe11c23fae8b5e5b06f9fe952c99ca46abaec8bda70a678cd0314dde"
SONIC_KEY_SINGLE="0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7"

# ── Helper ──────────────────────────────────────────────────────────────────
check_rpc() {
    local name="$1" url="$2"
    if curl -sf "$url" -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' > /dev/null 2>&1; then
        echo "  $name: OK"
        return 0
    else
        echo "  $name: UNREACHABLE"
        return 1
    fi
}

run_bench() {
    local chain="$1" rpc="$2" ws="$3" chain_id="$4" keys="$5" mode="$6" extra_args="$7"
    local out_file="${RUN_DIR}/${chain}_${mode}.json"

    echo ""
    echo "━━━ ${chain} — ${mode} ━━━"

    BENCH_KEY="$keys" cargo run -p evm-benchmark --release \
        --manifest-path "${BENCH_REPO_ROOT}/Cargo.toml" -- \
        --rpc-endpoints "$rpc" \
        --ws "$ws" \
        --chain-id "$chain_id" \
        --bench-name "${chain}_${mode}" \
        --out "$out_file" \
        --quiet \
        $extra_args 2>&1

    # Extract key metrics
    if [ -f "$out_file" ]; then
        python3 -c "
import json, sys
with open('$out_file') as f:
    r = json.load(f)['results']
print(f'  Submitted:     {r[\"submitted\"]}')
print(f'  Confirmed:     {r[\"confirmed\"]}')
print(f'  Confirmed TPS: {r[\"confirmed_tps\"]:.1f}')
print(f'  Latency p50:   {r[\"latency\"][\"p50\"]}ms')
print(f'  Latency p95:   {r[\"latency\"][\"p95\"]}ms')
print(f'  Latency p99:   {r[\"latency\"][\"p99\"]}ms')
" 2>/dev/null || echo "  (report parsing failed)"
    fi
}

# ── Pre-flight checks ──────────────────────────────────────────────────────
echo "╔══════════════════════════════════════════════════╗"
echo "║    Diesis vs Sonic — Comparative Benchmark      ║"
echo "╚══════════════════════════════════════════════════╝"
echo ""
echo "Checking endpoints..."

DIESIS_UP=true

for port in 8545 8555 8565 8575; do
    check_rpc "Diesis :$port" "http://localhost:$port" || DIESIS_UP=false
done
check_rpc "Sonic  node-1" "http://localhost:18545" || true  # Sonic may be stopped initially

if [ "$DIESIS_UP" = false ]; then
    echo ""
    echo "ERROR: Diesis is not running. Start it with:"
    echo "  make e2e-up-release"
    exit 1
fi

echo ""
echo "Diesis running. Sonic will be started/stopped as needed."
echo "Each chain runs in isolation to avoid CPU/memory contention."
echo "Results directory: ${RUN_DIR}"

# ── Build harness once ─────────────────────────────────────────────────────
echo ""
echo "Building evm-benchmark (release)..."
cargo build -p evm-benchmark --release --manifest-path "${BENCH_REPO_ROOT}/Cargo.toml" 2>&1 | tail -1

# ── Phase 1: Diesis benchmarks (Sonic stopped) ────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════"
echo "  PHASE 1: DIESIS (stopping Sonic to avoid resource contention)"
echo "═══════════════════════════════════════════════════"
docker compose -f "${SCRIPT_DIR}/chains/sonic/docker-compose.yml" stop 2>/dev/null || true
sleep 2

echo ""
echo "── Burst: ${BURST_TXS} transactions ──"
run_bench "diesis" "$DIESIS_RPC" "$DIESIS_WS" "$DIESIS_CHAIN_ID" "$DIESIS_KEYS" "burst" \
    "--execution burst --txs ${BURST_TXS} --batch-size ${BATCH_SIZE} --senders ${NUM_SENDERS} --fund"

echo ""
echo "── Sustained: ${SUSTAINED_TPS} TPS × ${SUSTAINED_DURATION}s ──"
run_bench "diesis" "$DIESIS_RPC" "$DIESIS_WS" "$DIESIS_CHAIN_ID" "$DIESIS_KEYS" "sustained" \
    "--execution sustained --tps ${SUSTAINED_TPS} --duration ${SUSTAINED_DURATION} --senders ${NUM_SENDERS}"

# ── Phase 2: Sonic benchmarks (Diesis stopped) ────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════"
echo "  PHASE 2: SONIC (stopping Diesis to avoid resource contention)"
echo "═══════════════════════════════════════════════════"

# Stop Diesis
DIESIS_BLOCK_PERIOD='100ms' DIESIS_ORDERING_WINDOW='15ms' DIESIS_MIN_ROUND_DELAY='10ms' \
DIESIS_MAX_BLOCK_TX_COUNT='5000' DIESIS_MAX_EXECUTION_LAG='32' DIESIS_PARALLEL_EXECUTION='full' \
DIESIS_COMMITMENT_MODE='verkle' DIESIS_SKIP_STATE_ROOT_VALIDATION='false' \
DIESIS_TXPOOL_MAX_ACCOUNT_SLOTS='5000' DIESIS_NONVALIDATOR_MSG_BUDGET='100000' \
DIESIS_NONVALIDATOR_RATE_WINDOW_MS='1000' DIESIS_MAX_PROPOSAL_TX_COUNT='1024' \
DIESIS_MAX_GAS_PER_PROPOSAL='30000000' DOCKERFILE='docker/Dockerfile' \
docker compose -f "${DIESIS_REPO_ROOT}/docker/docker-compose.e2e.yml" stop 2>/dev/null || true
sleep 2

# Restart Sonic
docker compose -f "${SCRIPT_DIR}/chains/sonic/docker-compose.yml" start 2>/dev/null || true
echo "Waiting for Sonic to recover..."
sleep 10

# Re-connect peers (they may have lost connections after stop/start)
bash "${SCRIPT_DIR}/chains/sonic/connect-peers.sh" 2>&1 | tail -5
sleep 5

echo ""
echo "── Burst: ${BURST_TXS} transactions ──"
run_bench "sonic" "$SONIC_RPC" "$SONIC_WS" "$SONIC_CHAIN_ID" "$SONIC_KEYS" "burst" \
    "--execution burst --txs ${BURST_TXS} --batch-size ${BATCH_SIZE} --senders ${NUM_SENDERS} --fund"

echo ""
echo "── Sustained: ${SUSTAINED_TPS} TPS × ${SUSTAINED_DURATION}s ──"
run_bench "sonic" "$SONIC_RPC" "$SONIC_WS" "$SONIC_CHAIN_ID" "$SONIC_KEYS" "sustained" \
    "--execution sustained --tps ${SUSTAINED_TPS} --duration ${SUSTAINED_DURATION} --senders ${NUM_SENDERS}"

# ── Restart both for further testing ──────────────────────────────────────
echo ""
echo "Restarting both chains for interactive use..."
DIESIS_BLOCK_PERIOD='100ms' DIESIS_ORDERING_WINDOW='15ms' DIESIS_MIN_ROUND_DELAY='10ms' \
DIESIS_MAX_BLOCK_TX_COUNT='5000' DIESIS_MAX_EXECUTION_LAG='32' DIESIS_PARALLEL_EXECUTION='full' \
DIESIS_COMMITMENT_MODE='verkle' DIESIS_SKIP_STATE_ROOT_VALIDATION='false' \
DIESIS_TXPOOL_MAX_ACCOUNT_SLOTS='5000' DIESIS_NONVALIDATOR_MSG_BUDGET='100000' \
DIESIS_NONVALIDATOR_RATE_WINDOW_MS='1000' DIESIS_MAX_PROPOSAL_TX_COUNT='1024' \
DIESIS_MAX_GAS_PER_PROPOSAL='30000000' DOCKERFILE='docker/Dockerfile' \
docker compose -f "${DIESIS_REPO_ROOT}/docker/docker-compose.e2e.yml" start 2>/dev/null || true

# ── Comparison report ─────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════"
echo "  GENERATING COMPARISON REPORT"
echo "═══════════════════════════════════════════════════"

python3 - "${RUN_DIR}" << 'PYEOF'
import json, sys, os, glob

run_dir = sys.argv[1]
reports = {}

for f in sorted(glob.glob(os.path.join(run_dir, "*.json"))):
    name = os.path.basename(f).replace(".json", "")
    with open(f) as fh:
        reports[name] = json.load(fh)

if not reports:
    print("No reports found!")
    sys.exit(1)

# Header
print("")
print("╔══════════════════════════════════════════════════════════════════════════╗")
print("║                  DIESIS vs SONIC — COMPARISON REPORT                   ║")
print("╚══════════════════════════════════════════════════════════════════════════╝")
print("")

# Build comparison table
modes = ["burst", "sustained"]
chains = ["diesis", "sonic"]

for mode in modes:
    d_key = f"diesis_{mode}"
    s_key = f"sonic_{mode}"
    if d_key not in reports or s_key not in reports:
        continue

    d = reports[d_key]["results"]
    s = reports[s_key]["results"]

    print(f"┌─ {mode.upper()} MODE {'─' * (60 - len(mode))}┐")
    print(f"│ {'Metric':<25} {'Diesis':>15} {'Sonic':>15} {'Delta':>12} │")
    print(f"│ {'─' * 67} │")

    def row(label, dv, sv, unit="", lower_better=False):
        if isinstance(dv, float):
            ds, ss = f"{dv:.1f}{unit}", f"{sv:.1f}{unit}"
            if dv > 0:
                pct = ((sv - dv) / dv) * 100
            else:
                pct = 0
        else:
            ds, ss = f"{dv}{unit}", f"{sv}{unit}"
            if dv > 0:
                pct = ((sv - dv) / dv) * 100
            else:
                pct = 0
        sign = "+" if pct >= 0 else ""
        # For latency, lower is better
        if lower_better:
            indicator = "✓" if pct <= 0 else "✗"
        else:
            indicator = "✓" if pct >= 0 else "✗"
        print(f"│ {label:<25} {ds:>15} {ss:>15} {sign}{pct:>6.1f}% {indicator}  │")

    row("Submitted",        d["submitted"],     s["submitted"])
    row("Confirmed",        d["confirmed"],     s["confirmed"])
    row("Confirmed TPS",    d["confirmed_tps"], s["confirmed_tps"])
    row("Submitted TPS",    d["submitted_tps"], s["submitted_tps"])
    row("Latency p50",      d["latency"]["p50"], s["latency"]["p50"], "ms", lower_better=True)
    row("Latency p95",      d["latency"]["p95"], s["latency"]["p95"], "ms", lower_better=True)
    row("Latency p99",      d["latency"]["p99"], s["latency"]["p99"], "ms", lower_better=True)
    row("Sign time",        d.get("sign_ms", 0), s.get("sign_ms", 0), "ms", lower_better=True)
    row("Submit time",      d.get("submit_ms", 0), s.get("submit_ms", 0), "ms", lower_better=True)
    row("Confirm time",     d.get("confirm_ms", 0), s.get("confirm_ms", 0), "ms", lower_better=True)

    pending_d = d["submitted"] - d["confirmed"]
    pending_s = s["submitted"] - s["confirmed"]
    row("Dropped/Pending",  pending_d, pending_s, "", lower_better=True)
    print(f"└{'─' * 69}┘")
    print("")

# Write markdown report
md_path = os.path.join(run_dir, "comparison.md")
with open(md_path, "w") as f:
    f.write("# Diesis vs Sonic — Benchmark Comparison\n\n")
    f.write(f"**Date**: {reports[list(reports.keys())[0]]['captured_at'][:10]}\n")
    f.write(f"**Hardware**: Same machine, Docker containers\n\n")

    for mode in modes:
        d_key = f"diesis_{mode}"
        s_key = f"sonic_{mode}"
        if d_key not in reports or s_key not in reports:
            continue
        d = reports[d_key]
        s = reports[s_key]
        dr = d["results"]
        sr = s["results"]

        f.write(f"## {mode.title()} Mode\n\n")
        f.write(f"| Metric | Diesis | Sonic |\n")
        f.write(f"|--------|--------|-------|\n")
        f.write(f"| Chain ID | {d['chain_id']} | {s['chain_id']} |\n")
        f.write(f"| Txs Submitted | {dr['submitted']} | {sr['submitted']} |\n")
        f.write(f"| Txs Confirmed | {dr['confirmed']} | {sr['confirmed']} |\n")
        f.write(f"| **Confirmed TPS** | **{dr['confirmed_tps']:.1f}** | **{sr['confirmed_tps']:.1f}** |\n")
        f.write(f"| Submitted TPS | {dr['submitted_tps']:.1f} | {sr['submitted_tps']:.1f} |\n")
        f.write(f"| Latency p50 | {dr['latency']['p50']}ms | {sr['latency']['p50']}ms |\n")
        f.write(f"| Latency p95 | {dr['latency']['p95']}ms | {sr['latency']['p95']}ms |\n")
        f.write(f"| Latency p99 | {dr['latency']['p99']}ms | {sr['latency']['p99']}ms |\n")
        f.write(f"| Sign time | {dr.get('sign_ms', 0)}ms | {sr.get('sign_ms', 0)}ms |\n")
        f.write(f"| Submit time | {dr.get('submit_ms', 0)}ms | {sr.get('submit_ms', 0)}ms |\n")
        f.write(f"| Confirm time | {dr.get('confirm_ms', 0)}ms | {sr.get('confirm_ms', 0)}ms |\n")
        f.write(f"\n")

    f.write("## Configuration\n\n")
    f.write("### Diesis\n")
    f.write("- 4 validators, 150ms block period, Verkle commitment\n")
    f.write("- txpool: 5000 account slots, 50000 pending max\n")
    f.write("- Parallel execution: full\n\n")
    f.write("### Sonic\n")
    f.write("- 4 validators, DAG-based Lachesis BFT (fakenet)\n")
    f.write("- txpool: 256 account slots, 5000 global slots\n")
    f.write("- Cache: 6144 MB\n")
    f.write("- Min base fee: 50 GWei\n")

print(f"Markdown report: {md_path}")
PYEOF

echo ""
echo "All results saved to: ${RUN_DIR}/"
echo "Done!"
