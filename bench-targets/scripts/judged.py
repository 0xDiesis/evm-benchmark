#!/usr/bin/env python3
"""The judged Diesis run: an aged fixture (past 5,000 blocks) at 2 CPU / 4 GiB per validator,
one 600 s window at a set offered rate (docs/diesis-methodology.md).

  judged.py --fixture <name> --rate 2000 [--profile capped] [--window 600] [--warmup 60] [--max-load 18]
  judged.py --tag <aged fleet> --rate 2000 ...

--fixture restores a saved fixture (fleet.py save-fixture) into a throwaway aged fleet, runs,
and destroys the copy, so the fixture itself is never modified. --tag runs on an existing
stopped aged fleet in place (the chain keeps aging). Either way the fleet starts under
--profile. If a validator refuses to start, the run is
recorded as refused, with each validator's error and, for Work admission, the bytes requested
and the configured limit, and no load is offered. Otherwise bench.sh drives --warmup + --window
seconds of sustained load at --rate against the running fleet (DIESIS_FLEET=external, routed
through the fixture's port block) and the observer measures the window. The fleet is stopped
afterwards. Results: $BENCH_RESULTS_BASE/judged/<stamp>-<tag>-<profile>-<rate>/judged.json and
judged.md, reporting finalized TPS, submit->finalized p50/p95/p99, block interval p50/p99,
per-container CPU (cores) and peak memory, validator agreement, and host load. A window whose
1-minute host load exceeded --max-load is marked functional-only.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FLEET = ROOT / "bench-targets" / "chains" / "diesis" / "fleet.py"
WORK = re.compile(r"(?P<what>[A-Za-z ]*Work admission): (?P<kind>\w+) requires (?P<req>\d+), configured limit is (?P<lim>\d+)")
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def results_base():
    return Path(os.environ.get("BENCH_RESULTS_BASE", ROOT / "bench-targets" / "results"))


def refusal(failed_dir):
    """Per-validator first error and exit reason from fleet.py's failed-start capture."""
    nodes, work = {}, None
    for log in sorted(failed_dir.glob("node-*.log")):
        text = ANSI.sub("", log.read_text(errors="replace"))
        lines = text.splitlines()
        first = next((l for l in lines if " ERROR " in l or "panicked" in l), None)
        reason = next((l for l in reversed(lines) if "shutting down due to error" in l or l.startswith("Error:")), None)
        state_path = log.with_suffix(".state.json")
        state = json.loads(state_path.read_text()) if state_path.exists() and state_path.read_text().strip() else {}
        nodes[log.stem] = dict(first_error=first, exit_reason=reason, exit_code=state.get("ExitCode"),
                               oom_killed=state.get("OOMKilled"))
        m = WORK.search(text)
        if m and work is None:
            work = dict(node=log.stem, admission=m["what"].strip(), kind=m["kind"], requested_bytes=int(m["req"]),
                        limit_bytes=int(m["lim"]))
    return dict(nodes=nodes, work_admission=work)


def summary(m, max_load):
    f, r, iv = m["submit_to_finalized_s"], m["submit_to_receipt_s"], m["block_interval_ms"]
    load = m.get("host_load_1m") or {}
    agree = m.get("finalized_agreement") or {}
    return dict(
        finalized_tps=m["finalized_tps"], attempted_tps=m["attempted_tps"],
        submit_to_finalized_s={k: f.get(k) for k in ("p50", "p95", "p99")},
        submit_to_receipt_s={k: r.get(k) for k in ("p50", "p95", "p99")},
        unfinalized_share=m["unfinalized_share"],
        block_interval_ms={k: iv.get(k) for k in ("p50", "p99")},
        cpu_cores_nodes=m.get("cpu_cores_nodes"), peak_memory_bytes_nodes=m.get("peak_memory_bytes_nodes"),
        finalized_agreement=agree.get("agree"), agreement_heights=[h["height"] for h in agree.get("heights", [])],
        validator_exits=m.get("validator_exits"), generator_bound=m.get("generator_bound"),
        chain_saturated=m.get("chain_saturated"), host_load_1m=load,
        functional_only=(load.get("max") or 0) > max_load)


def markdown(record):
    lines = [f"# Judged run {record.get('fixture') or record['tag']} ({record['profile']}, {record['rate']} TPS offered)", "",
             f"- Status: **{record['status']}**", f"- Image: {record.get('image')}",
             f"- Fixture height at start: {record.get('fixture_height')}"]
    if record["status"] == "refused":
        work = record["refusal"].get("work_admission")
        if work:
            lines.append(f"- Work admission: {work['admission']} on {work['node']} requested "
                         f"{work['requested_bytes']:,} bytes against a limit of {work['limit_bytes']:,} ({work['kind']})")
        for node, info in record["refusal"]["nodes"].items():
            lines.append(f"- {node}: exit {info['exit_code']} (oom {info['oom_killed']}): {info['exit_reason']}")
    elif record["status"] == "measured":
        s = record["summary"]
        fmt = lambda d: " / ".join("-" if v is None else f"{v:.2f}" for v in d.values())
        lines += [
            f"- Finalized TPS: {s['finalized_tps']:.1f} (attempted {s['attempted_tps']:.1f})",
            f"- Submit→finalized p50/p95/p99 s: {fmt(s['submit_to_finalized_s'])} (unfinalized {s['unfinalized_share']:.3f})",
            f"- Submit→receipt p50/p95/p99 s: {fmt(s['submit_to_receipt_s'])}",
            f"- Block interval p50/p99 ms: {fmt(s['block_interval_ms'])}",
            f"- CPU cores per validator: " + ", ".join(f"{k} {v:.2f}" for k, v in (s['cpu_cores_nodes'] or {}).items()),
            f"- Peak memory per validator: " + ", ".join(f"{k} {v / 2**30:.2f} GiB"
                                                          for k, v in (s['peak_memory_bytes_nodes'] or {}).items()),
            f"- Validator agreement: {s['finalized_agreement']} at {s['agreement_heights']}; exits {s['validator_exits']}",
            f"- Host load (1 min): {s['host_load_1m']}" + ("  **functional-only**" if s["functional_only"] else ""),
        ]
    return "\n".join(lines) + "\n"


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    source = p.add_mutually_exclusive_group(required=True)
    source.add_argument("--fixture", help="Saved fixture name (restored into a throwaway copy)")
    source.add_argument("--tag", help="Existing stopped aged fleet, used in place")
    p.add_argument("--rate", type=int, required=True)
    p.add_argument("--profile", choices=["capped", "uncapped"], default="capped")
    p.add_argument("--window", type=int, default=600)
    p.add_argument("--warmup", type=int, default=60)
    p.add_argument("--max-load", type=float, default=18.0)
    a = p.parse_args()
    restored = None
    if a.fixture:
        restored = f"j{time.strftime('%m%d%H%M%S')}"
        subprocess.run([sys.executable, "-B", str(FLEET), "restore-fixture", "--fixture", a.fixture, "--as", restored],
                       check=True)
        a.tag = restored
    manifest = json.loads((results_base() / "fleet" / f"aged-{a.tag}.json").read_text())
    project = manifest["project"]
    out = results_base() / "judged" / f"{time.strftime('%Y%m%d-%H%M%S')}-{a.fixture or a.tag}-{a.profile}-{a.rate}"
    out.mkdir(parents=True)
    env = dict(os.environ, DIESIS_AGED_TAG=a.tag, BENCH_DIESIS_PROFILE=a.profile)
    record = dict(tag=a.tag, fixture=a.fixture, restored_copy=restored, profile=a.profile, rate=a.rate,
                  window_s=a.window, warmup_s=a.warmup,
                  fixture_height=manifest.get("height"), genesis_hash=manifest.get("genesis_hash"),
                  image=(manifest.get("provenance") or {}).get("image"), started=time.time())
    state = results_base() / "fleet" / project
    before = set(state.glob("failed-start-*"))
    up = subprocess.run([sys.executable, "-B", str(FLEET), "up"], env=env, capture_output=True, text=True)
    (out / "fleet-up.log").write_text(up.stdout + up.stderr)
    try:
        if up.returncode:
            failed = sorted(set(state.glob("failed-start-*")) - before)
            record.update(status="refused", refusal=refusal(failed[-1]) if failed else dict(nodes={}, work_admission=None),
                          failed_start=str(failed[-1]) if failed else None)
        else:
            run_dir = out / "run"
            bench = subprocess.run(["bash", str(HERE / "bench.sh"), "--chain", "diesis", "--mode", "sustained",
                                    "--tps", str(a.rate), "--duration", str(a.window + a.warmup), "--out", str(run_dir),
                                    "--tag", f"judged-{a.tag}"],
                                   env=dict(env, DIESIS_FLEET="external", BENCH_WARMUP_SECS=str(a.warmup)),
                                   stdout=(out / "bench.log").open("w"), stderr=subprocess.STDOUT)
            metrics = run_dir / "diesis-metrics.json"
            if metrics.exists():
                record.update(status="measured", summary=summary(json.loads(metrics.read_text()), a.max_load),
                              bench_exit=bench.returncode)
            else:
                record.update(status="failed", bench_exit=bench.returncode)
    finally:
        subprocess.run([sys.executable, "-B", str(FLEET), "down"], env=env, capture_output=True, text=True)
        if restored:
            subprocess.run([sys.executable, "-B", str(FLEET), "destroy-aged", "--tag", restored], capture_output=True,
                           text=True)
        record["ended"] = time.time()
        (out / "judged.json").write_text(json.dumps(record, indent=2) + "\n")
        (out / "judged.md").write_text(markdown(record))
        print((out / "judged.md").read_text())
    return 0 if record["status"] == "measured" else 2


if __name__ == "__main__":
    sys.exit(main())
