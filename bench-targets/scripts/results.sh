#!/bin/bash
# results.sh — Utility for listing and looking up benchmark results.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "${SCRIPT_DIR}/lib.sh"  # shellcheck source=lib.sh
INDEX_FILE="${RESULTS_BASE}/index.json"
RUNS_DIR="${RESULTS_BASE}/runs"

usage() { cat <<'EOF'
Usage: results.sh [command] [options]

Commands:
  list [--chain <name>] [--mode <mode>] [--limit <n>]   List runs (default: all, limit 20)
  latest [--chain <name>] [--mode <mode>]                Show latest run details
  show <run_path>                                        Show full details of a specific run
  compare <path1> <path2> [<path3>...]                   Ad-hoc compare 2+ run report.json files
  summary                                                Show aggregate stats across all runs
EOF
exit 0; }

load_index() {
    if [ -f "${INDEX_FILE}" ]; then cat "${INDEX_FILE}"; else
        python3 -c "
import json,glob,os
runs_dir='${RUNS_DIR}'; entries=[]
for rpt in sorted(glob.glob(os.path.join(runs_dir,'*','*','*','report.json'))):
    d=os.path.dirname(rpt); parts=os.path.relpath(d,runs_dir).split(os.sep)
    if len(parts)<3: continue
    chain,mode,ts=parts[0],parts[1],parts[2]
    try:
        r=json.load(open(rpt)); res=r.get('results',{}); lat=res.get('latency',{})
        sub,conf=res.get('submitted',0),res.get('confirmed',0)
        mp=os.path.join(d,'meta.json'); env_v=json.load(open(mp)).get('env','') if os.path.isfile(mp) else ''
        entries.append(dict(path=d,chain=chain,mode=mode,env=env_v,tps=res.get('confirmed_tps',0),
            p50=lat.get('p50',0),p99=lat.get('p99',0),
            confirmed_rate=round(conf/sub*100,1) if sub else 0,
            timestamp=ts.split('_')[0] if '_' in ts else ts))
    except Exception: pass
print(json.dumps(entries))"
    fi
}

cmd_list() {
    local chain="" mode="" limit=20
    while [[ $# -gt 0 ]]; do case "$1" in
        --chain) chain="$2"; shift 2;; --mode) mode="$2"; shift 2;;
        --limit) limit="$2"; shift 2;; *) log_error "Unknown: $1"; exit 1;; esac; done
    load_index | python3 -c "
import json,sys; data=json.load(sys.stdin)
cf,mf,lim='${chain}','${mode}',int('${limit}')
if cf: data=[r for r in data if r['chain']==cf]
if mf: data=[r for r in data if r['mode']==mf]
data.sort(key=lambda r:r.get('timestamp',''),reverse=True); data=data[:lim]
if not data: print('No runs found.'); sys.exit(0)
h=f\"{'#':<4}{'Chain':<12}{'Mode':<10}{'Env':<10}{'TPS':>8}{'p50':>8}{'p99':>8}{'Conf%':>7} {'Timestamp':<16}\"
print(h); print('-'*len(h))
for i,r in enumerate(data,1):
    t=f\"{r['tps']:.1f}\" if isinstance(r['tps'],float) else str(r['tps'])
    print(f\"{i:<4}{r['chain']:<12}{r['mode']:<10}{r.get('env',''):<10}{t:>8}{str(r['p50'])+'ms':>8}{str(r['p99'])+'ms':>8}{str(round(r.get('confirmed_rate',0)))+'%':>7} {r.get('timestamp',''):<16}\")"
}

cmd_latest() {
    local chain="" mode=""
    while [[ $# -gt 0 ]]; do case "$1" in
        --chain) chain="$2"; shift 2;; --mode) mode="$2"; shift 2;;
        *) log_error "Unknown: $1"; exit 1;; esac; done
    local target="${RUNS_DIR}/latest"
    [[ -n "${chain}" ]] && target="${RUNS_DIR}/${chain}/latest"
    [[ -n "${chain}" && -n "${mode}" ]] && target="${RUNS_DIR}/${chain}/${mode}/latest"
    [[ ! -e "${target}" ]] && { log_error "No latest symlink at ${target}"; exit 1; }
    _pretty_print_run "$(cd "${target}" && pwd)"
}

cmd_show() {
    local p="${1:?Usage: results.sh show <run_path>}"
    [[ -f "$p" && "$(basename "$p")" == "report.json" ]] && p="$(dirname "$p")"
    [[ ! -d "$p" ]] && { log_error "Not found: $p"; exit 1; }
    _pretty_print_run "$p"
}

cmd_compare() {
    [[ $# -lt 2 ]] && { log_error "compare requires 2+ paths"; exit 1; }
    local reports=()
    for p in "$@"; do
        if [[ -d "$p" ]]; then reports+=("${p}/report.json")
        elif [[ -f "$p" ]]; then reports+=("$p")
        else log_error "Not found: $p"; exit 1; fi
    done
    python3 - "${reports[@]}" <<'PYEOF'
import json,sys,os
runs=[]
for p in sys.argv[1:]:
    d=json.load(open(p)); label=d.get('benchmark',os.path.basename(os.path.dirname(p)))
    res=d.get('results',{}); lat=res.get('latency',{}); sub=res.get('submitted',0); conf=res.get('confirmed',0)
    runs.append(dict(label=label,submitted=sub,confirmed=conf,confirmed_tps=res.get('confirmed_tps',0),
        submitted_tps=res.get('submitted_tps',0),p50=lat.get('p50',0),p95=lat.get('p95',0),
        p99=lat.get('p99',0),avg=lat.get('avg',0),conf_rate=round(conf/sub*100,1) if sub else 0))
w=14; cols=[r['label'] for r in runs]
h=f"{'Metric':<20}"+''.join(f"{c:>{w}}" for c in cols)+(f"{'Delta':>{w}}" if len(runs)>1 else '')
print(h); print('-'*len(h))
def fmt(v,s=''): return f"{v:.1f}{s}" if isinstance(v,float) else f"{v}{s}"
def delta(b,c):
    if not b: return '-'
    p=(c-b)/abs(b)*100; return f"{'+' if p>=0 else ''}{p:.1f}%"
for name,key,sfx in [('Submitted','submitted',''),('Confirmed','confirmed',''),
    ('Confirmed TPS','confirmed_tps',''),('Submitted TPS','submitted_tps',''),
    ('Confirm %','conf_rate','%'),('Latency p50','p50','ms'),('Latency p95','p95','ms'),
    ('Latency p99','p99','ms'),('Latency avg','avg','ms')]:
    vals=[r[key] for r in runs]; row=f"{name:<20}"+''.join(f"{fmt(v,sfx):>{w}}" for v in vals)
    if len(runs)>1: row+=f"{delta(vals[0],vals[-1]):>{w}}"
    print(row)
PYEOF
}

cmd_summary() {
    load_index | python3 -c "
import json,sys
from collections import defaultdict
data=json.load(sys.stdin)
if not data: print('No runs found.'); sys.exit(0)
g=defaultdict(list)
for r in data: g[r['chain']].append(r)
h=f\"{'Chain':<12}{'Runs':>5}{'Avg TPS':>9}{'Best TPS':>9}{'Avg p50':>9}{'Best p50':>9} {'Last Run':<16}\"
print(h); print('-'*len(h))
for c in sorted(g):
    runs=g[c]; tv=[r['tps'] for r in runs if r['tps']]; pv=[r['p50'] for r in runs if r['p50']]
    at=sum(tv)/len(tv) if tv else 0; bt=max(tv) if tv else 0
    ap=sum(pv)/len(pv) if pv else 0; bp=min(pv) if pv else 0
    lr=sorted([r.get('timestamp','') for r in runs],reverse=True)
    print(f\"{c:<12}{len(runs):>5}{at:>9.1f}{bt:>9.1f}{ap:>8.0f}ms{bp:>8.0f}ms {lr[0] if lr else '-':<16}\")"
}

_pretty_print_run() {
    local run_dir="$1"
    [[ ! -f "${run_dir}/report.json" ]] && { log_error "No report.json in ${run_dir}"; exit 1; }
    python3 - "${run_dir}" <<'PYEOF'
import json,os,sys
d=sys.argv[1]; R='\033[0m'; C='\033[1;36m'; G='\033[1;32m'; Y='\033[1;33m'; B='\033[1;34m'
def sec(t): print(f"\n{C}--- {t} ---{R}")
def kv(k,v,c=G): print(f"  {c}{k:<22}{R} {v}")
report=json.load(open(os.path.join(d,'report.json')))
mp=os.path.join(d,'meta.json'); meta=json.load(open(mp)) if os.path.isfile(mp) else {}
name=report.get('benchmark',os.path.basename(d))
print(f"\n{C}{'='*50}{R}\n{C}  {name}{R}\n{C}{'='*50}{R}")
if meta:
    sec('Metadata')
    for k in ('timestamp','chain','mode','env','git_sha','git_branch','harness_version'):
        if k in meta: kv(k,meta[k],B)
    if 'system' in meta:
        s=meta['system']; kv('system',f"{s.get('os','')} {s.get('arch','')} ({s.get('cpus','')} CPUs, {s.get('memory_gb','')} GB)",B)
sec('Config')
for k,v in report.get('config',{}).items(): kv(k,v)
sec('Results'); res=report.get('results',{}); sub=res.get('submitted',0); conf=res.get('confirmed',0)
rate=f"{conf/sub*100:.1f}%" if sub else '-'
kv('Submitted',sub); kv('Confirmed',f"{conf} ({rate})")
kv('Submitted TPS',f"{res.get('submitted_tps',0):.1f}"); kv('Confirmed TPS',f"{res.get('confirmed_tps',0):.1f}",Y)
sec('Latency'); lat=res.get('latency',{})
for k in ('p50','p95','p99','min','max','avg'):
    if k in lat: kv(k,f"{lat[k]}ms")
if res.get('per_wave'):
    sec('Per-Wave'); wh=f"  {'Wave':<6}{'Count':>6}{'p50':>8}{'p95':>8}{'p99':>8}{'Max':>8}"
    print(wh); print('  '+'-'*(len(wh)-2))
    for w in res['per_wave']: print(f"  {w['wave']:<6}{w['count']:>6}{w['p50']:>7}ms{w['p95']:>7}ms{w['p99']:>7}ms{w['max']:>7}ms")
print()
PYEOF
}

# ── Main dispatch ───────────────────────────────────────────────────────
cmd="${1:-}"; shift || true
case "${cmd}" in
    list) cmd_list "$@";; latest) cmd_latest "$@";; show) cmd_show "$@";;
    compare) cmd_compare "$@";; summary) cmd_summary;;
    -h|--help|help|"") usage;; *) log_error "Unknown command: ${cmd}"; usage;;
esac
