#!/usr/bin/env python3
"""The judged Diesis run: an aged fixture (past 5,000 blocks) at 2 CPU / 4 GiB per validator,
one 600 s window at a set offered rate (docs/diesis-methodology.md).

  judged.py --fixture <name> --rate 2000 --outcome-rule bls-only [--mix judged] [--profile capped]
            [--window 600] [--warmup 60] [--max-load 18]
  judged.py --tag <aged fleet> --rate 2000 --outcome-rule bls-only ...

--outcome-rule pins the genesis outcome signature rule the run is judged under (fleets default
BLS-only, the node genesis defaults hybrid); it must match the rule the fixture was built with, or
the run is refused before the fleet starts. --mix judged (the default) drives the pinned judged
mixed-EVM workload (crates/evm-benchmark/src/generators/judged_mix.rs); --mix transfer drives plain
transfers. Finalized TPS counts successful receipts only.

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
1-minute host load exceeded --max-load is marked functional-only. Judged runs run alone: the fleet
starts only while no other Diesis fleet runs (fleet.py waits), each validator's cpuset is recorded,
and a window with another Diesis fleet up at its start or end is marked functional-only.
"""
import argparse
import importlib.util
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


def load_fleet():
    spec = importlib.util.spec_from_file_location("fleet", FLEET)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def mark_shared(summary, at_start, at_end):
    """A judged window runs alone: another Diesis fleet seen at its start or end makes it
    functional-only."""
    shared = bool(at_start or at_end)
    return dict(summary, shared_host=shared, functional_only=summary["functional_only"] or shared)


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


def check_outcome_rule(requested, recorded):
    """The judged outcome signature rule: explicit, and the one the fixture's genesis was built with."""
    if recorded is None:
        raise SystemExit("the fixture does not record its genesis outcome signature rule; rebuild it with a "
                         "checkout that has the rule control")
    if requested != recorded:
        raise SystemExit(f"--outcome-rule {requested} does not match the fixture's genesis rule {recorded}")
    return requested


def summary(m, max_load):
    f, r, iv = m["submit_to_finalized_s"], m["submit_to_receipt_s"], m["block_interval_ms"]
    load = m.get("host_load_1m") or {}
    agree = m.get("finalized_agreement") or {}
    return dict(
        finalized_tps=m["finalized_tps"], attempted_tps=m["attempted_tps"],
        finalized_basis=m.get("finalized_basis", "included"), finalized_included_tps=m.get("finalized_included_tps"),
        outcomes=m.get("outcomes"), reverted_share=m.get("reverted_share"), per_type=m.get("per_type"),
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
             f"- Outcome signature rule: {record.get('outcome_signature_rule')} (pinned)",
             f"- Mix: {record.get('mix')}",
             f"- Fixture height at start: {record.get('fixture_height')}"]
    if record.get("cpusets"):
        lines.append("- Validator cpusets: " + ", ".join(f"{k} {v}" for k, v in record["cpusets"].items()))
    if "other_fleets_at_end" in record:
        lines.append(f"- Other Diesis fleets at the window's start / end: {record.get('other_fleets_at_start') or 'none'}"
                     f" / {record['other_fleets_at_end'] or 'none'}")
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
            f"- Finalized TPS: {s['finalized_tps']:.1f} ({s.get('finalized_basis', 'included')} transactions; "
            f"attempted {s['attempted_tps']:.1f})",
            f"- Submit→finalized p50/p95/p99 s: {fmt(s['submit_to_finalized_s'])} (unfinalized {s['unfinalized_share']:.3f})",
            f"- Submit→receipt p50/p95/p99 s: {fmt(s['submit_to_receipt_s'])}",
            f"- Block interval p50/p99 ms: {fmt(s['block_interval_ms'])}",
            f"- CPU cores per validator: " + ", ".join(f"{k} {v:.2f}" for k, v in (s['cpu_cores_nodes'] or {}).items()),
            f"- Peak memory per validator: " + ", ".join(f"{k} {v / 2**30:.2f} GiB"
                                                          for k, v in (s['peak_memory_bytes_nodes'] or {}).items()),
            f"- Validator agreement: {s['finalized_agreement']} at {s['agreement_heights']}; exits {s['validator_exits']}",
            f"- Host load (1 min): {s['host_load_1m']}" + ("  **functional-only**" if s["functional_only"] else "")
            + ("  (another fleet shared the host)" if s.get("shared_host") else ""),
        ]
        o = s.get("outcomes") or {}
        if o.get("finalized_reverted") is not None:
            lines.append(f"- Finalized outcomes: successful {o['finalized_successful']}, reverted "
                         f"{o['finalized_reverted']}, status unknown {o['finalized_status_unknown']}; reverted share of "
                         f"window submissions {s.get('reverted_share') or 0:.4f}")
        types_ = s.get("per_type") or {}
        if types_ and set(types_) != {"unknown"}:
            num = lambda v, f="{:.2f}": "-" if v is None else f.format(v)
            lines += ["", "| Type | Share | Successful TPS | Reverted | Submit→finalized p50/p99 s | "
                      "Submit→receipt p50 s | Gas used | Fills |", "|---|---|---|---|---|---|---|---|"]
            for name, t in types_.items():
                lines.append(f"| {name} | {num(t['share_of_successful'], '{:.3f}')} | "
                             f"{num(t['finalized_successful_tps'], '{:.1f}')} | {t['finalized_reverted']} | "
                             f"{num(t['submit_to_finalized_s']['p50'])} / {num(t['submit_to_finalized_s']['p99'])} | "
                             f"{num(t['submit_to_receipt_s']['p50'])} | {t['gas_used_avg']} | {t['fills']} |")
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
    p.add_argument("--outcome-rule", choices=["bls-only", "hybrid"], required=True,
                   help="Genesis outcome signature rule the run is judged under; must match the fixture")
    p.add_argument("--mix", choices=["judged", "transfer"], default="judged")
    a = p.parse_args()
    restored = None
    if a.fixture:
        restored = f"j{time.strftime('%m%d%H%M%S')}"
        subprocess.run([sys.executable, "-B", str(FLEET), "restore-fixture", "--fixture", a.fixture, "--as", restored],
                       check=True)
        a.tag = restored
    manifest = json.loads((results_base() / "fleet" / f"aged-{a.tag}.json").read_text())
    project = manifest["project"]
    try:
        rule = check_outcome_rule(a.outcome_rule, (manifest.get("provenance") or {}).get("outcome_signature_rule"))
    except SystemExit:
        if restored:
            subprocess.run([sys.executable, "-B", str(FLEET), "destroy-aged", "--tag", restored], capture_output=True,
                           text=True)
        raise
    out = results_base() / "judged" / f"{time.strftime('%Y%m%d-%H%M%S')}-{a.fixture or a.tag}-{a.profile}-{a.rate}"
    out.mkdir(parents=True)
    # Judged runs run alone: the fleet starts only while no other Diesis fleet runs (fleet.py waits).
    env = dict(os.environ, DIESIS_AGED_TAG=a.tag, BENCH_DIESIS_PROFILE=a.profile, BENCH_DIESIS_MAX_FLEETS="1")
    record = dict(tag=a.tag, fixture=a.fixture, restored_copy=restored, profile=a.profile, rate=a.rate,
                  outcome_signature_rule=rule, mix=a.mix,
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
            fleet = load_fleet()
            provenance = json.loads((state / "provenance.json").read_text())
            record.update(cpu_base=provenance.get("cpu_base"), cpusets=provenance.get("cpusets"),
                          other_fleets_at_start=sorted(fleet.running_fleets(exclude={project})))
            run_dir = out / "run"
            bench = subprocess.run(["bash", str(HERE / "bench.sh"), "--chain", "diesis", "--mode", "sustained",
                                    "--tps", str(a.rate), "--duration", str(a.window + a.warmup), "--out", str(run_dir),
                                    "--tag", f"judged-{a.tag}"] + (["--mix", "judged"] if a.mix == "judged" else []),
                                   env=dict(env, DIESIS_FLEET="external", BENCH_WARMUP_SECS=str(a.warmup)),
                                   stdout=(out / "bench.log").open("w"), stderr=subprocess.STDOUT)
            record["other_fleets_at_end"] = sorted(fleet.running_fleets(exclude={project}))
            mix_record = run_dir / "judged-mix.json"
            if mix_record.exists():
                record["mix_record"] = json.loads(mix_record.read_text())
            metrics = run_dir / "diesis-metrics.json"
            if metrics.exists():
                record.update(status="measured", bench_exit=bench.returncode, summary=mark_shared(
                    summary(json.loads(metrics.read_text()), a.max_load), record["other_fleets_at_start"],
                    record["other_fleets_at_end"]))
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
