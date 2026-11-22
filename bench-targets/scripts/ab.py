#!/usr/bin/env python3
"""Interleaved A/B (or repeated baseline) runs of the Diesis benchmark, with medians and ranges.

  ab.py --name NAME --a "profile=capped" [--b "profile=capped,image=<tag>,env=DIESIS_X=1+DIESIS_Y=2"] \
      [--arms 3] [--rates 2000] [--duration 660] [--warmup 60] [--aged TAG]

A side is a comma list of:
  profile=capped|uncapped   resource profile (chains/diesis/profiles/)
  image=<tag>               diesis-node-e2e tag (DIESIS_IMAGE_TAG); default: checkout HEAD-release
  repo=<path>               node checkout to render (must match the image); default DIESIS_REPO_DIR
  env=K=V+K=V               validator overrides (BENCH_DIESIS_ENV)
  outcome=hybrid|bls-only   genesis outcome signature rule (BENCH_DIESIS_OUTCOME_RULE); default:
                            the harness default for the checkout
  name=<short>              label in the summary
With --b the arms run A B B A A B ... until each side has --arms; without it, A repeats. --note
labels the results: one line at the top of summary.md, for example a setting common to both sides.
Each arm runs one sustained bench.sh step per rate (fresh genesis per step, or a restart of
the aged fleet --aged on its own volumes). --probe TPS:SECONDS adds a low-rate idle probe as
the arm's first step; --reuse-fleet runs the arm's later steps on the fleet its first step
started (DIESIS_FLEET=external). --max-load reruns an arm, on a fresh fleet, when any of its
windows saw a 1-minute host load above the limit (--max-reruns, default 1); an arm still
over the limit after that is kept and counted as functional-only in the summary. Before each arm the host must be quiet for 60 s
(1-minute load below 8, no compiler or docker build) within 10 minutes; the outcome is
recorded, never forced: no exclusive measurement window is taken. Re-running with the same
--name skips completed steps and retries failed ones once. Results:
$BENCH_RESULTS_BASE/ab/<name>/{plan.json, arm-<i><side>/<rate>/, summary.json, summary.md}.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import statistics
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
BUILD = re.compile(r"(^|/)(cargo|rustc|cargo-nextest|clang|forge|solc)( |$)|docker (buildx )?build")


def results_base():
    return Path(os.environ.get("BENCH_RESULTS_BASE", ROOT / "bench-targets" / "results"))


def parse_side(text):
    side = {}
    for part in [p for p in text.split(",") if p]:
        key, sep, value = part.partition("=")
        if not sep or key not in {"profile", "image", "repo", "env", "outcome", "name"}:
            raise SystemExit(f"Bad side field {part!r}")
        side[key] = value
    if side.get("profile", "capped") not in {"capped", "uncapped"}:
        raise SystemExit("profile must be capped or uncapped")
    if side.get("outcome", "hybrid") not in {"hybrid", "bls-only"}:
        raise SystemExit("outcome must be hybrid or bls-only")
    side.setdefault("profile", "capped")
    side["env"] = [e for e in side.get("env", "").split("+") if e]
    for item in side["env"]:
        if not re.fullmatch(r"(DIESIS|RAYON)_[A-Z0-9_]+=.*", item):
            raise SystemExit(f"Bad override {item!r}")
    return side


def schedule(arms, two_sided):
    if not two_sided:
        return ["a"] * arms
    order, pattern = [], "abba"
    while order.count("a") < arms or order.count("b") < arms:
        order.append(pattern[len(order) % 4])
    return order


def wait_quiet(limit=600, quiet=60):
    deadline, since, builds = time.monotonic() + limit, None, []
    while time.monotonic() < deadline:
        ps = subprocess.run(["ps", "-axo", "command="], capture_output=True, text=True).stdout
        builds = [line[:120] for line in ps.splitlines() if BUILD.search(line)]
        clear = os.getloadavg()[0] < 8 and not builds
        since = (since or time.monotonic()) if clear else None
        if since and time.monotonic() - since >= quiet:
            return dict(quiet=True, load=os.getloadavg()[0])
        time.sleep(5)
    return dict(quiet=False, load=os.getloadavg()[0], builds=builds[:3])


METRICS = [
    ("finalized_tps", lambda s: s["finalized_tps"]),
    ("finalized_p50_s", lambda s: s["submit_to_finalized_s"]["p50"]),
    ("finalized_p95_s", lambda s: s["submit_to_finalized_s"]["p95"]),
    ("finalized_p99_s", lambda s: s["submit_to_finalized_s"]["p99"]),
    ("receipt_p50_s", lambda s: s["submit_to_receipt_s"]["p50"]),
    ("receipt_p95_s", lambda s: s["submit_to_receipt_s"]["p95"]),
    ("receipt_p99_s", lambda s: s["submit_to_receipt_s"]["p99"]),
    ("unfinalized_share", lambda s: s["unfinalized_share"]),
    ("attempted_tps", lambda s: s["attempted_tps"]),
    ("latest_to_finalized_p50_s", lambda s: s["latest_to_finalized_s"]["p50"]),
    ("latest_to_finalized_p99_s", lambda s: s["latest_to_finalized_s"]["p99"]),
    ("interval_p50_ms", lambda s: s["block_interval_ms"]["p50"]),
    ("interval_p99_ms", lambda s: s["block_interval_ms"]["p99"]),
    ("tx_per_block", lambda s: s["per_block_median"]["tx_per_block"]),
    ("rounds_per_block", lambda s: s["per_block_median"]["rounds_per_block"]),
    ("hold_ms_per_block", lambda s: s["per_block_median"]["hold_ms_per_block"]),
    ("hold_ms_5s_p99", lambda s: s["hold_ms_per_block_5s"]["p99"]),
    ("quorum_wait_ms_per_block", lambda s: s["per_block_median"]["quorum_wait_ms_per_block"]),
    ("quorum_wait_ms_p99", lambda s: s["quorum_wait_ms_exporter"].get("0.99")),
    ("cpu_ms_per_tx", lambda s: s["cpu_s_per_tx_median"] * 1000),
    ("pool_slope_tx_per_s", lambda s: s["pool_slope_tx_per_s"]),
    ("empty_block_share", lambda s: s["empty_block_share"]),
    ("unknown_parent_vertex", lambda s: sum(c["unknown_parent_vertex"] for c in s["log_counts"].values())),
    ("work_refusals", lambda s: sum(n for c in s["log_counts"].values() for by in c["work_refused"].values()
                                    for n in by.values())),
    ("peak_memory_gib_max", lambda s: max(s["peak_memory_bytes_nodes"].values(), default=None) / 2**30),
    ("host_load_median", lambda s: s["host_load_1m"]["median"]),
]


def value(fn, step):
    try:
        return fn(step)
    except (KeyError, TypeError):
        return None


def step_metrics(out, i, key, step):
    path = out / f"arm-{i}{key}" / str(step) / "diesis-metrics.json"
    return json.loads(path.read_text()) if path.exists() else None


def steps_of(plan):
    """[(name, tps, duration, warmup)] for one arm."""
    steps = []
    if plan.get("probe"):
        tps, seconds = (int(x) for x in plan["probe"].split(":"))
        steps.append(("probe", tps, seconds, 0))
    steps += [(str(rate), rate, plan.get("duration"), plan.get("warmup")) for rate in plan["rates"]]
    return steps


def summarize(out, plan):
    summary = {}
    for key, side in plan["sides"].items():
        arms = [i for i, k in enumerate(plan["order"], 1) if k == key]
        summary[key] = dict(config=side, rates={})
        for rate, *_ in steps_of(plan):
            steps = [m for i in arms if (m := step_metrics(out, i, key, rate))]
            row = dict(n=len(steps))
            for name, fn in METRICS:
                vals = [v for v in (value(fn, s) for s in steps) if v is not None]
                row[name] = dict(median=statistics.median(vals), min=min(vals), max=max(vals), n=len(vals)) if vals else None
            for verdict in ("generator_bound", "chain_saturated"):
                row[verdict] = sum(bool(s.get(verdict)) for s in steps)
            row["pool_full"] = sum(s.get("pool_full_at_s") is not None for s in steps)
            row["functional_only"] = sum((out / f"arm-{i}{key}" / "functional-only.json").exists() for i in arms
                                         if step_metrics(out, i, key, rate))
            row["finalized_agreement"] = sum(bool((s.get("finalized_agreement") or {}).get("agree")) for s in steps)
            summary[key]["rates"][str(rate)] = row
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    cols = [name for name, _ in METRICS]

    def fmt(cell):
        if not cell:
            return "-"
        f = (lambda v: f"{v:.0f}") if abs(cell["median"]) >= 100 else (lambda v: f"{v:.2f}")
        return f"{f(cell['median'])} ({f(cell['min'])}–{f(cell['max'])})"

    lines = [f"# {out.name} ({time.strftime('%Y-%m-%d %H:%M %Z')})", ""]
    if plan.get("note"):
        lines += [f"> {plan['note']}", ""]
    lines += ["Median (min–max) over arms; verdict columns count arms. A `-` in a submit→finalized percentile "
              "means more than that share never finalized within the drain (see unfinalized_share).", ""]
    for key, data in summary.items():
        c = data["config"]
        lines += [f"## {key.upper()}: {c.get('name') or c['profile']} — {c['profile']} image={c.get('image') or 'HEAD'} "
                  f"{'outcome=' + c['outcome'] + ' ' if c.get('outcome') else ''}{' '.join(c['env'])}", "",
                  "| offered | " + " | ".join(cols) + " | gen-bound | saturated | pool full | agree | functional-only |",
                  "|---" * (len(cols) + 6) + "|"]
        for rate, row in data["rates"].items():
            lines.append(f"| {rate} | " + " | ".join(fmt(row[c]) for c in cols)
                         + f" | {row['generator_bound']}/{row['n']} | {row['chain_saturated']}/{row['n']}"
                         f" | {row['pool_full']}/{row['n']} | {row['finalized_agreement']}/{row['n']}"
                         f" | {row['functional_only']}/{row['n']} |")
        lines.append("")
    (out / "summary.md").write_text("\n".join(lines) + "\n")


def run_step(out, i, key, side, step, plan, log, external=False, aged=None):
    name, rate, duration, warmup = step
    step_dir = out / f"arm-{i}{key}" / name
    if (step_dir / "diesis-metrics.json").exists():
        return True
    if step_dir.exists():
        step_dir.rename(step_dir.with_name(f"{name}-failed-{int(time.time())}"))
    env = dict(os.environ, BENCH_DIESIS_PROFILE=side["profile"], BENCH_WARMUP_SECS=str(warmup),
               BENCH_DIESIS_ENV=" ".join(shlex.quote(e) for e in side["env"]))
    if side.get("image"):
        env["DIESIS_IMAGE_TAG"] = side["image"]
    if side.get("repo"):
        env["DIESIS_REPO_DIR"] = side["repo"]
    if side.get("outcome"):
        env["BENCH_DIESIS_OUTCOME_RULE"] = side["outcome"]
    if aged or plan.get("aged"):
        env["DIESIS_AGED_TAG"] = aged or plan["aged"]
    if external:
        env["DIESIS_FLEET"] = "external"
    cmd = ["bash", str(HERE / "bench.sh"), "--chain", "diesis", "--mode", "sustained", "--tps", str(rate),
           "--duration", str(duration), "--out", str(step_dir), "--tag", f"{out.name}-{i}{key}"]
    log.write(f"{time.strftime('%H:%M:%S')} arm {i}{key} {name}{' (same fleet)' if external else ''}: "
              f"{' '.join(cmd)}\n")
    log.flush()
    result = subprocess.run(cmd, env=env, stdout=log, stderr=subprocess.STDOUT)
    return result.returncode == 0 and (step_dir / "diesis-metrics.json").exists()


def run_arm(out, i, key, plan, log, attempt, no_quiet_wait):
    """Run the arm's missing steps; returns whether anything ran."""
    side = plan["sides"][key]
    steps = steps_of(plan)
    pending = [st for st in steps if not step_metrics(out, i, key, st[0])]
    if not pending:
        return False
    host = dict(quiet=None) if no_quiet_wait else wait_quiet()
    arm_dir = out / f"arm-{i}{key}"
    arm_dir.mkdir(exist_ok=True)
    (arm_dir / f"host-{attempt}.json").write_text(json.dumps(host) + "\n")
    # A saved fixture is restored into a throwaway aged fleet for this arm and destroyed after,
    # so every arm starts from the same fixture and the fixture itself is never modified.
    aged = None
    fleet = str(HERE.parent / "chains" / "diesis" / "fleet.py")
    if plan.get("fixture"):
        aged = f"ab{int(plan['created']) % 100000}-{i}{key}"
        subprocess.run([sys.executable, "-B", fleet, "destroy-aged", "--tag", aged], capture_output=True)
        restore = subprocess.run([sys.executable, "-B", fleet, "restore-fixture", "--fixture", plan["fixture"],
                                  "--as", aged], stdout=log, stderr=subprocess.STDOUT)
        if restore.returncode:
            log.write(f"{time.strftime('%H:%M:%S')} arm {i}{key}: fixture restore failed\n")
            return True
    try:
        # The first step run in this pass starts a fresh fleet; with --reuse-fleet the rest use it.
        for n, step in enumerate(pending):
            external = bool(plan.get("reuse_fleet")) and n > 0
            ok = run_step(out, i, key, side, step, plan, log, external=external, aged=aged)
            log.write(f"{time.strftime('%H:%M:%S')} arm {i}{key} {step[0]}: {'ok' if ok else 'FAILED'}\n")
            log.flush()
            if not ok and plan.get("reuse_fleet"):
                break
    finally:
        if aged:
            subprocess.run([sys.executable, "-B", fleet, "destroy-aged", "--tag", aged], capture_output=True)
    return True


def overloaded(out, i, key, plan):
    """The highest 1-minute host load over the arm's windows when above --max-load."""
    if not plan.get("max_load"):
        return None
    loads = [(m.get("host_load_1m") or {}).get("max") or 0 for st in steps_of(plan)
             if (m := step_metrics(out, i, key, st[0]))]
    worst = max(loads, default=0)
    return worst if worst > plan["max_load"] else None


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--name", required=True, help="[a-z0-9-]+")
    p.add_argument("--a", default="profile=capped")
    p.add_argument("--b")
    p.add_argument("--arms", type=int, default=3)
    p.add_argument("--rates", default="2000")
    p.add_argument("--duration", type=int, default=660, help="Load seconds per step (warmup + window)")
    p.add_argument("--warmup", type=int, default=60)
    p.add_argument("--aged", help="Aged fleet tag (fleet.py age), used in place; default fresh genesis per step")
    p.add_argument("--fixture", help="Saved fixture (fleet.py save-fixture), restored into a fresh copy per arm")
    p.add_argument("--probe", help="TPS:SECONDS idle probe as each arm's first step, e.g. 2:30")
    p.add_argument("--reuse-fleet", action="store_true", help="Run each arm's later steps on its first step's fleet")
    p.add_argument("--max-load", type=float, help="Rerun an arm whose windows saw a higher 1-minute host load")
    p.add_argument("--max-reruns", type=int, default=1,
                   help="Reruns per overloaded arm; one still overloaded is kept and marked functional-only")
    p.add_argument("--no-quiet-wait", action="store_true")
    p.add_argument("--note", help="One line printed at the top of summary.md (kept in plan.json)")
    p.add_argument("--summarize-only", action="store_true")
    a = p.parse_args()
    if not re.fullmatch(r"[a-z0-9-]+", a.name):
        raise SystemExit("--name must match [a-z0-9-]+")
    out = results_base() / "ab" / a.name
    out.mkdir(parents=True, exist_ok=True)
    plan_path = out / "plan.json"
    if plan_path.exists():
        plan = json.loads(plan_path.read_text())
    else:
        sides = {"a": parse_side(a.a)} | ({"b": parse_side(a.b)} if a.b else {})
        if a.probe and not re.fullmatch(r"\d+:\d+", a.probe):
            raise SystemExit("--probe must be TPS:SECONDS")
        plan = dict(sides=sides, order=schedule(a.arms, bool(a.b)), rates=[int(r) for r in a.rates.split(",")],
                    duration=a.duration, warmup=a.warmup, aged=a.aged, fixture=a.fixture, probe=a.probe,
                    reuse_fleet=a.reuse_fleet, note=a.note,
                    max_load=a.max_load, max_reruns=a.max_reruns, created=time.time())
        plan_path.write_text(json.dumps(plan, indent=2) + "\n")
    if a.summarize_only:
        return summarize(out, plan)
    with (out / "ab.log").open("a") as log:
        for attempt in (1, 2):
            for i, key in enumerate(plan["order"], 1):
                reruns = plan.get("max_reruns", 1)
                for rerun in range(reruns + 1):
                    if not run_arm(out, i, key, plan, log, attempt, a.no_quiet_wait):
                        break
                    hot = overloaded(out, i, key, plan)
                    if hot and rerun == reruns:
                        # Kept, but not a performance result: the host stayed busy.
                        (out / f"arm-{i}{key}" / "functional-only.json").write_text(
                            json.dumps(dict(host_load_max=hot, limit=plan["max_load"])) + "\n")
                        log.write(f"{time.strftime('%H:%M:%S')} arm {i}{key} host load {hot:.0f} above "
                                  f"{plan['max_load']} after {reruns} rerun(s): kept as functional-only\n")
                    if not hot or rerun == reruns:
                        break
                    arm_dir = out / f"arm-{i}{key}"
                    log.write(f"{time.strftime('%H:%M:%S')} arm {i}{key} host load {hot:.0f} above "
                              f"{plan['max_load']}: rerunning\n")
                    arm_dir.rename(arm_dir.with_name(f"{arm_dir.name}-overloaded-{int(time.time())}"))
                summarize(out, plan)
    summarize(out, plan)
    print(out / "summary.md")


if __name__ == "__main__":
    sys.exit(main())
