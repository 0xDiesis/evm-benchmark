#!/bin/bash
# compare.sh — Multi-chain comparison orchestrator.
# Runs identical benchmarks across multiple chains in isolation (one at a time),
# then generates a side-by-side comparison report.
#
# Usage:
#   ./bench-targets/scripts/compare.sh --chains "diesis sonic" [options]
set -euo pipefail

SCRIPTS_DIR="$(cd "$(dirname "$0")" && pwd)"

if [[ ! -f "${SCRIPTS_DIR}/lib.sh" ]]; then
    echo "ERROR: lib.sh not found at ${SCRIPTS_DIR}/lib.sh" >&2; exit 1
fi
# shellcheck source=lib.sh
source "${SCRIPTS_DIR}/lib.sh"

# ── Defaults ─────────────────────────────────────────────────────────────────
CHAINS="" MODE="burst" ENV="clean"
TXS=2000 TPS=200 DURATION=30 SENDERS=200 BATCH_SIZE=200
FUND="true"

# ── Argument parsing ─────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --chains)      CHAINS="$2";      shift 2 ;;
        --mode)        MODE="$2";        shift 2 ;;
        --env)         ENV="$2";         shift 2 ;;
        --txs)         TXS="$2";         shift 2 ;;
        --tps)         TPS="$2";         shift 2 ;;
        --duration)    DURATION="$2";    shift 2 ;;
        --senders)     SENDERS="$2";     shift 2 ;;
        --batch-size)  BATCH_SIZE="$2";  shift 2 ;;
        --no-fund)     FUND="false";     shift ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo "  --chains \"c1 c2\"    Space-separated chain names (required)"
            echo "  --mode <mode>       burst|sustained|ceiling|all (default: burst)"
            echo "  --env <env>         clean|geo-global|etc (default: clean)"
            echo "  --txs/--tps/--duration/--senders/--batch-size: numeric params"
            echo "  --no-fund           Skip auto-funding (use pre-funded chain keys)"
            exit 0 ;;
        *) log_error "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ -z "${CHAINS}" ]]; then
    log_error "--chains is required (e.g. --chains \"diesis sonic\")"; exit 1
fi

read -ra CHAIN_LIST <<< "${CHAINS}"
for chain in "${CHAIN_LIST[@]}"; do
    if ! load_chain_config "${chain}" > /dev/null 2>&1; then
        log_error "Unknown chain: '${chain}'. Available: $(list_chains)"; exit 1
    fi
done

if [[ "${MODE}" == "all" ]]; then MODES="burst sustained ceiling"; else MODES="${MODE}"; fi

log_header "Multi-Chain Comparison: ${CHAINS// / vs }"
log_info "Chains: ${CHAINS}  |  Modes: ${MODES}  |  Env: ${ENV}"
log_info "Params: txs=${TXS} tps=${TPS} duration=${DURATION} senders=${SENDERS} batch=${BATCH_SIZE}"

# ── Stop a chain by name ─────────────────────────────────────────────────────
stop_chain() {
    local name="$1"
    load_chain_config "${name}"
    if [[ -n "${CHAIN_DOWN_CMD}" ]]; then
        log_info "Stopping ${name}..."
        eval "${CHAIN_DOWN_CMD}" > /dev/null 2>&1 || true
    fi
    # Wait for the primary RPC to stop responding (max 15s) so we don't start
    # the next chain while this one still holds a port.
    local primary_rpc="${CHAIN_RPC%%,*}"
    if [[ -n "${primary_rpc}" ]]; then
        local waited=0
        while check_rpc "${primary_rpc}" 2>/dev/null && [[ ${waited} -lt 15 ]]; do
            sleep 1; waited=$((waited + 1))
        done
    fi
}

chain_ready_timeout() {
    local name="$1"
    local upper
    upper="$(echo "${name}" | tr '[:lower:]-' '[:upper:]_')"
    local specific_var="BENCH_CHAIN_READY_TIMEOUT_SECS_${upper}"

    # Priority:
    # 1) Per-chain override (e.g. BENCH_CHAIN_READY_TIMEOUT_SECS_BERACHAIN)
    # 2) Global override (BENCH_CHAIN_READY_TIMEOUT_SECS)
    # 3) Chain defaults
    if [[ -n "${!specific_var:-}" ]]; then
        echo "${!specific_var}"
        return 0
    fi
    if [[ -n "${BENCH_CHAIN_READY_TIMEOUT_SECS:-}" ]]; then
        echo "${BENCH_CHAIN_READY_TIMEOUT_SECS}"
        return 0
    fi

    case "${name}" in
        berachain|bsc|cosmos|sei)
            # These chains often require longer startup/bootstrap windows.
            echo "300"
            ;;
        *)
            echo "120"
            ;;
    esac
}

print_redacted_log_tail() {
    local file="$1"
    local lines="${2:-20}"
    if [[ ! -f "${file}" ]]; then
        return 0
    fi
    tail -"${lines}" "${file}" | sed -E \
        -e 's/("password"[[:space:]]*:[[:space:]]*")[^"]+"/\1<redacted>"/g' \
        -e 's/("auth"[[:space:]]*:[[:space:]]*")[^"]+"/\1<redacted>"/g' \
        -e 's/(Authorization:[[:space:]]*Bearer[[:space:]]+)[^[:space:]]+/\1<redacted>/g'
}

ensure_chain_running() {
    local name="$1"
    local ready_timeout primary_rpc startup_log

    load_chain_config "${name}"
    primary_rpc="${CHAIN_RPC%%,*}"
    ready_timeout="$(chain_ready_timeout "${name}")"

    if check_rpc "${primary_rpc}"; then
        log_info "${name} is already responding at ${primary_rpc}"
        # Verify it's the right chain by checking chain ID
        local actual_chain_id
        actual_chain_id=$(curl -sf "${primary_rpc}" -X POST -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' 2>/dev/null | \
            python3 -c "import sys,json; print(int(json.load(sys.stdin).get('result','0x0'),16))" 2>/dev/null || echo "0")
        if [[ -n "${CHAIN_CHAIN_ID:-}" && "${actual_chain_id}" != "${CHAIN_CHAIN_ID}" && "${actual_chain_id}" != "0" ]]; then
            log_warn "${name} RPC at ${primary_rpc} has chain ID ${actual_chain_id}, expected ${CHAIN_CHAIN_ID} — restarting"
            eval "${CHAIN_DOWN_CMD:-true}" > /dev/null 2>&1 || true
            sleep 3
        else
            return 0
        fi
    fi

    if [[ -z "${CHAIN_UP_CMD}" ]]; then
        log_error "No start command configured for ${name}"
        return 1
    fi

    log_info "Starting ${name}..."
    mkdir -p "${RESULTS_BASE}/logs"
    startup_log="${RESULTS_BASE}/logs/startup_${name}_$(date +%Y%m%d-%H%M%S).log"
    if ! eval "${CHAIN_UP_CMD}" > "${startup_log}" 2>&1; then
        log_error "${name} start command failed. Last startup log lines:"
        print_redacted_log_tail "${startup_log}" 20 >&2 || true
        return 1
    fi

    # Some chains (e.g. Berachain) discover dynamic RPC ports during startup.
    # Reload config to pick up any newly generated endpoint mappings.
    load_chain_config "${name}"
    primary_rpc="${CHAIN_RPC%%,*}"

    if [[ -z "${primary_rpc}" ]]; then
        log_error "${name} has no RPC endpoint configured after startup"
        return 1
    fi

    wait_for_chain "${primary_rpc}" "${ready_timeout}"
    # Wait for consensus to produce blocks before handing off to bench.sh.
    # bench.sh will restart the chain with geo overrides and do its own check,
    # but for chains without CHAIN_CLEAN_CMD (no restart in bench.sh), this
    # ensures the chain is actively producing blocks before benchmarking.
    wait_for_block_advance "${primary_rpc}" 60
}

# ── Run one mode across all chains ───────────────────────────────────────────
run_mode() {
    local mode="$1" chains_tag="${CHAINS// /_}"
    local COMP_DIR failed_chains=()
    local mode_upper
    mode_upper="$(echo "${mode}" | tr '[:lower:]' '[:upper:]')"
    COMP_DIR="$(make_comparison_dir "${chains_tag}" "${mode}")"
    log_header "${mode_upper} mode -- output: ${COMP_DIR}"
    log_info "Starting mode '${mode}' across chains: ${CHAINS}"

    for chain in "${CHAIN_LIST[@]}"; do
        log_info "=== Starting ${chain} (${mode}) ==="

        # Stop all OTHER chains for isolation (stop_chain waits for port to free)
        for other in "${CHAIN_LIST[@]}"; do
            [[ "${other}" != "${chain}" ]] && { stop_chain "${other}" || true; }
        done

        if ! ensure_chain_running "${chain}"; then
            log_error "=== Failed ${chain} (${mode}) before benchmark start ==="
            failed_chains+=("${chain}")
            continue
        fi

        local bench_args=(
            --chain "${chain}" --mode "${mode}" --env "${ENV}"
            --txs "${TXS}" --tps "${TPS}" --duration "${DURATION}"
            --senders "${SENDERS}" --batch-size "${BATCH_SIZE}"
        )
        [[ "${FUND}" == "false" ]] && bench_args+=(--no-fund)

        local bench_output run_dir
        if bench_output=$("${SCRIPTS_DIR}/bench.sh" "${bench_args[@]}" 2>&1); then
            run_dir=$(echo "${bench_output}" | grep "^Run directory:" | tail -1 | sed 's/^Run directory: //')
            if [[ -z "${run_dir}" || ! -d "${run_dir}" ]]; then
                log_error "Could not determine run directory for ${chain}"
                echo "${bench_output}" | tail -10 >&2
                failed_chains+=("${chain}"); continue
            fi
            if [[ -f "${run_dir}/report.json" ]]; then
                cp "${run_dir}/report.json" "${COMP_DIR}/${chain}.json"
                log_info "Copied ${chain} report to ${COMP_DIR}/${chain}.json"
                log_info "=== Completed ${chain} (${mode}) successfully ==="
            else
                log_error "No report.json in ${run_dir} for ${chain}"
                failed_chains+=("${chain}"); continue
            fi
            [[ -f "${run_dir}/meta.json" ]] && cp "${run_dir}/meta.json" "${COMP_DIR}/${chain}-meta.json"
        else
            log_error "=== Failed ${chain} (${mode}) ==="
            log_error "bench.sh failed for ${chain} (${mode})"
            echo "${bench_output:-}" | tail -20 >&2
            failed_chains+=("${chain}"); continue
        fi
    done

    local result_count
    result_count=$(find "${COMP_DIR}" -maxdepth 1 -name "*.json" ! -name "*-meta.json" ! -name "summary.json" | wc -l | tr -d ' ')
    if [[ "${result_count}" -lt 2 ]]; then
        log_warn "Only ${result_count} chain(s) produced results -- skipping comparison"
        [[ ${#failed_chains[@]} -gt 0 ]] && log_warn "Failed: ${failed_chains[*]}"
        log_warn "Mode '${mode}' did not produce a full comparison set."
        return 1
    fi

    generate_comparison "${COMP_DIR}" "${mode}" "${CHAIN_LIST[*]}"
    update_latest_symlink "${COMP_DIR}"
    [[ ${#failed_chains[@]} -gt 0 ]] && log_warn "Some chains failed: ${failed_chains[*]}"
    log_info "Finished mode '${mode}'."
    return 0
}

# ── Generate comparison report (summary.json, summary.md, console table) ─────
generate_comparison() {
    local comp_dir="$1" mode="$2" chains_str="$3"
    python3 - "${comp_dir}" "${mode}" "${chains_str}" << 'PYEOF'
import json, sys, os

comp_dir, mode, chains_str = sys.argv[1], sys.argv[2], sys.argv[3]
chain_order = chains_str.split()

reports = {}
for c in chain_order:
    p = os.path.join(comp_dir, f"{c}.json")
    if os.path.exists(p):
        with open(p) as f: reports[c] = json.load(f)

chains = [c for c in chain_order if c in reports]
if len(chains) < 2:
    print("Not enough reports for comparison", file=sys.stderr); sys.exit(1)
base = chains[0]

def R(c):
    """Get results dict for chain."""
    return reports[c].get("results", reports[c])

METRICS = [
    ("Submitted",        lambda r: r.get("submitted", 0),                          False),
    ("Confirmed",        lambda r: r.get("confirmed", 0),                          False),
    ("Confirmed TPS",    lambda r: r.get("confirmed_tps", 0.0),                    False),
    ("Submitted TPS",    lambda r: r.get("submitted_tps", 0.0),                    False),
    ("Latency p50 (ms)", lambda r: r.get("latency", {}).get("p50", 0),             True),
    ("Latency p95 (ms)", lambda r: r.get("latency", {}).get("p95", 0),             True),
    ("Latency p99 (ms)", lambda r: r.get("latency", {}).get("p99", 0),             True),
    ("Sign time (ms)",   lambda r: r.get("sign_ms", 0),                            True),
    ("Submit time (ms)", lambda r: r.get("submit_ms", 0),                          True),
    ("Confirm time (ms)",lambda r: r.get("confirm_ms", 0),                         True),
    ("Dropped/Pending",  lambda r: r.get("submitted", 0) - r.get("confirmed", 0),  True),
    ("Confirmed %",      lambda r: (r.get("confirmed",0)/r.get("submitted",1))*100,False),
]

def delta(bv, v, lower_better):
    if bv == 0: return "   --   "
    pct = ((v - bv) / bv) * 100
    sign = "+" if pct >= 0 else ""
    ok = "ok" if (pct <= 0 if lower_better else pct >= 0) else "!!"
    return f"{sign}{pct:>6.1f}% {ok}"

# ── summary.json ──
summary = {"mode": mode, "chains": {}}
for c in chains:
    r = R(c)
    summary["chains"][c] = {
        "submitted": r.get("submitted",0), "confirmed": r.get("confirmed",0),
        "confirmed_tps": r.get("confirmed_tps",0), "submitted_tps": r.get("submitted_tps",0),
        "latency_p50": r.get("latency",{}).get("p50",0),
        "latency_p95": r.get("latency",{}).get("p95",0),
        "latency_p99": r.get("latency",{}).get("p99",0),
        "sign_ms": r.get("sign_ms",0), "submit_ms": r.get("submit_ms",0),
        "confirm_ms": r.get("confirm_ms",0),
    }
with open(os.path.join(comp_dir, "summary.json"), "w") as f:
    json.dump(summary, f, indent=2); f.write("\n")

# ── Console table ──
LW, VW, DW = 22, 14, 12
sep = "-" * (LW + VW * len(chains) + DW * (len(chains)-1) + 4)

print(f"\n  COMPARISON: {' vs '.join(c.upper() for c in chains)} -- {mode.upper()}")
print(f"  {sep}")
hdr = f"  {'Metric':<{LW}}" + "".join(f"{c:>{VW}}" for c in chains)
hdr += "".join(f"{'vs '+base:>{DW}}" for _ in chains[1:])
print(hdr); print(f"  {sep}")

for label, fn, lb in METRICS:
    vals = [fn(R(c)) for c in chains]
    line = f"  {label:<{LW}}"
    for v in vals:
        line += f"{v:>{VW}.1f}" if isinstance(v, float) else f"{v:>{VW}}"
    for v in vals[1:]:
        line += f"{delta(vals[0], v, lb):>{DW}}"
    print(line)
print(f"  {sep}\n")

# ── summary.md ──
md = os.path.join(comp_dir, "summary.md")
cols = ["Metric"] + [c.title() for c in chains] + [f"vs {base.title()}" for _ in chains[1:]]
with open(md, "w") as f:
    f.write(f"# Comparison: {' vs '.join(c.title() for c in chains)} -- {mode.title()}\n\n")
    f.write("| " + " | ".join(cols) + " |\n")
    f.write("|" + "|".join("---" for _ in cols) + "|\n")
    for label, fn, lb in METRICS:
        vals = [fn(R(c)) for c in chains]
        vs = [f"{v:.1f}" if isinstance(v, float) else str(v) for v in vals]
        bold = "Confirmed TPS" in label
        if bold: vs = [f"**{s}**" for s in vs]
        ds = []
        for v in vals[1:]:
            if vals[0] == 0: ds.append("--")
            else:
                pct = ((v-vals[0])/vals[0])*100
                ds.append(f"{'+' if pct>=0 else ''}{pct:.1f}%")
        f.write("| " + " | ".join([label]+vs+ds) + " |\n")
    f.write("\n")

print(f"  Summary:  {os.path.join(comp_dir, 'summary.json')}")
print(f"  Markdown: {md}")
PYEOF
}

# ── Main loop ────────────────────────────────────────────────────────────────
OVERALL_EXIT=0
for mode in ${MODES}; do
    if ! run_mode "${mode}"; then
        log_warn "Mode '${mode}' had failures; continuing to next mode."; OVERALL_EXIT=1
    fi
done

# ── Stop all chains after comparison ─────────────────────────────────────────
log_info "Stopping all chains..."
for chain in "${CHAIN_LIST[@]}"; do
    load_chain_config "${chain}"
    [[ -n "${CHAIN_DOWN_CMD}" ]] && { eval "${CHAIN_DOWN_CMD}" > /dev/null 2>&1 || true; }
done

log_info "Comparison complete."
exit "${OVERALL_EXIT}"
