#!/bin/bash
# sweep.sh — Parameter sweep orchestrator.
# Iterates over values for a single E2E parameter, benchmarking each,
# then generates summary reports.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Source shared library
if [[ ! -f "${SCRIPT_DIR}/lib.sh" ]]; then
    echo "ERROR: lib.sh not found at ${SCRIPT_DIR}/lib.sh" >&2; exit 1
fi
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

# Source sweep profiles
if [[ ! -f "${SCRIPT_DIR}/sweep-profiles.sh" ]]; then
    echo "ERROR: sweep-profiles.sh not found at ${SCRIPT_DIR}/sweep-profiles.sh" >&2; exit 1
fi
# shellcheck source=sweep-profiles.sh
source "${SCRIPT_DIR}/sweep-profiles.sh"

# ── Defaults ─────────────────────────────────────────────────────────────
PARAM_PROFILE="" MODE="burst" TXS=2000 TPS=200 SENDERS=200 BATCH_SIZE=200
LIST_PROFILES="false"

# ── Argument parsing ────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --param)      PARAM_PROFILE="$2"; shift 2 ;;
        --mode)       MODE="$2";          shift 2 ;;
        --txs)        TXS="$2";           shift 2 ;;
        --tps)        TPS="$2";           shift 2 ;;
        --senders)    SENDERS="$2";       shift 2 ;;
        --batch-size) BATCH_SIZE="$2";    shift 2 ;;
        --list)       LIST_PROFILES="true"; shift ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo "  --param <profile>    Sweep profile name (required)"
            echo "  --mode <mode>        Benchmark mode (default: burst)"
            echo "  --txs <n>            Transaction count (default: 2000)"
            echo "  --tps <n>            Target TPS (default: 200)"
            echo "  --senders <n>        Senders (default: 200)"
            echo "  --batch-size <n>     Batch size (default: 200)"
            echo "  --list               List available sweep profiles"
            exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# ── List profiles ───────────────────────────────────────────────────────
if [[ "${LIST_PROFILES}" == "true" ]]; then
    list_sweep_profiles
    exit 0
fi

# ── Validate ────────────────────────────────────────────────────────────
if [[ -z "${PARAM_PROFILE}" ]]; then
    log_error "--param is required. Use --list to see available profiles."
    exit 1
fi

if ! load_sweep_profile "${PARAM_PROFILE}"; then
    exit 1
fi

log_header "Parameter Sweep: ${SWEEP_DESC}"
log_info "Parameter: ${SWEEP_PARAM}"
log_info "Values:    ${SWEEP_VALUES}"
log_info "Mode:      ${MODE}  |  TXS: ${TXS}  |  TPS: ${TPS}"

# ── Create sweep directory ──────────────────────────────────────────────
SWEEP_DIR="$(make_sweep_dir "${PARAM_PROFILE}")"
export SWEEP_DIR
log_info "Sweep directory: ${SWEEP_DIR}"

# ── Save sweep config ──────────────────────────────────────────────────
python3 -c "
import json
config = {
    'profile': '${PARAM_PROFILE}',
    'param': '${SWEEP_PARAM}',
    'desc': '${SWEEP_DESC}',
    'values': '${SWEEP_VALUES}'.split(),
    'mode': '${MODE}',
    'txs': ${TXS},
    'tps': ${TPS},
    'senders': ${SENDERS},
    'batch_size': ${BATCH_SIZE},
}
with open('${SWEEP_DIR}/config.json', 'w') as f:
    json.dump(config, f, indent=2)
"

# ── Load Diesis chain config for RPC URL ────────────────────────────────
load_chain_config diesis

# ── Sweep loop ──────────────────────────────────────────────────────────
FAILED_VALUES=()
for value in ${SWEEP_VALUES}; do
    log_header "Testing ${SWEEP_PARAM}=${value}"

    VALUE_DIR="${SWEEP_DIR}/${value}"
    mkdir -p "${VALUE_DIR}"

    # Export the parameter and restart Diesis
    export "${SWEEP_PARAM}=${value}"
    log_info "Stopping Diesis..."
    make -C "${DIESIS_REPO_DIR}" e2e-down 2>&1 | tail -3 || true
    log_info "Starting Diesis with ${SWEEP_PARAM}=${value}..."
    if ! make -C "${DIESIS_REPO_DIR}" e2e-up-release 2>&1 | tail -5; then
        log_error "Failed to start Diesis with ${SWEEP_PARAM}=${value}, skipping"
        FAILED_VALUES+=("${value}")
        continue
    fi

    # Wait for chain readiness
    RPC_URL="${CHAIN_RPC%%,*}"
    if ! wait_for_chain "${RPC_URL}" 120; then
        log_error "Chain not ready for ${SWEEP_PARAM}=${value}, skipping"
        FAILED_VALUES+=("${value}")
        continue
    fi

    # Run benchmark
    log_info "Running benchmark..."
    if ! bash "${SCRIPT_DIR}/bench.sh" \
        --chain diesis --mode "${MODE}" \
        --tag "sweep-${value}" \
        --txs "${TXS}" --tps "${TPS}" \
        --senders "${SENDERS}" --batch-size "${BATCH_SIZE}" \
        --out "${VALUE_DIR}" --no-quiet; then
        log_warn "Benchmark failed for ${SWEEP_PARAM}=${value}"
        FAILED_VALUES+=("${value}")
        continue
    fi

    log_info "Completed ${SWEEP_PARAM}=${value}"
done

# ── Generate summaries ──────────────────────────────────────────────────
log_header "Generating Summary"

python3 << 'PYEOF'
import json, os, sys, glob

sweep_dir = os.environ.get("SWEEP_DIR", "")
if not sweep_dir:
    sys.exit("SWEEP_DIR not set")

config_path = os.path.join(sweep_dir, "config.json")
with open(config_path) as f:
    config = json.load(f)

results = []
for value in config["values"]:
    report_path = os.path.join(sweep_dir, value, "report.json")
    if not os.path.isfile(report_path):
        continue
    try:
        with open(report_path) as f:
            report = json.load(f)
        r = report["results"]
        submitted = r.get("submitted", 0)
        confirmed = r.get("confirmed", 0)
        rate = (confirmed / submitted * 100) if submitted > 0 else 0
        results.append({
            "value": value,
            "tps": round(r.get("confirmed_tps", 0), 1),
            "p50": r.get("latency", {}).get("p50", 0),
            "p95": r.get("latency", {}).get("p95", 0),
            "p99": r.get("latency", {}).get("p99", 0),
            "confirmed_rate": round(rate, 1),
        })
    except Exception as e:
        print(f"  WARN: could not parse {report_path}: {e}", file=sys.stderr)

# Sort by TPS descending
results.sort(key=lambda x: x["tps"], reverse=True)

# Write summary.json
with open(os.path.join(sweep_dir, "summary.json"), "w") as f:
    json.dump(results, f, indent=2)

# Write summary.md
param = config["param"]
desc = config["desc"]
md_lines = [
    f"# Parameter Sweep: {desc}",
    f"**Parameter:** `{param}`  ",
    f"**Mode:** {config['mode']}  |  **TXS:** {config['txs']}  |  **TPS target:** {config['tps']}",
    "",
    "| Value | Confirmed TPS | p50 (ms) | p95 (ms) | p99 (ms) | Confirmed % |",
    "|-------|--------------|----------|----------|----------|-------------|",
]
for r in results:
    md_lines.append(
        f"| {r['value']} | {r['tps']} | {r['p50']} | {r['p95']} | {r['p99']} | {r['confirmed_rate']}% |"
    )
with open(os.path.join(sweep_dir, "summary.md"), "w") as f:
    f.write("\n".join(md_lines) + "\n")

# Create best symlink
if results:
    best_value = results[0]["value"]
    best_link = os.path.join(sweep_dir, "best")
    if os.path.islink(best_link):
        os.unlink(best_link)
    os.symlink(best_value, best_link)
    print(f"  Best: {param}={best_value} ({results[0]['tps']} TPS)")

# Print table
print()
print(f"  {'Value':<14} {'TPS':>10} {'p50':>8} {'p95':>8} {'p99':>8} {'Confirmed':>10}")
print(f"  {'-'*14} {'-'*10} {'-'*8} {'-'*8} {'-'*8} {'-'*10}")
for r in results:
    print(f"  {r['value']:<14} {r['tps']:>10} {r['p50']:>8} {r['p95']:>8} {r['p99']:>8} {r['confirmed_rate']:>9}%")
PYEOF

# ── Update latest symlink ───────────────────────────────────────────────
update_latest_symlink "${SWEEP_DIR}"

# ── Report failures ─────────────────────────────────────────────────────
if [[ ${#FAILED_VALUES[@]} -gt 0 ]]; then
    log_warn "Failed values: ${FAILED_VALUES[*]}"
fi

log_info "Sweep complete: ${SWEEP_DIR}"
log_info "Summary:        ${SWEEP_DIR}/summary.md"
