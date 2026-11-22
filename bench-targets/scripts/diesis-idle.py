#!/usr/bin/env python3
"""Idle behaviour of a Diesis fleet per DIESIS_IDLE_STRETCH value (docs/diesis-methodology.md).

  diesis-idle.py --stretch 4,1 [--arms 2] [--profile capped] [--settle 60] [--idle 300]
                 [--probe-idle 120] [--probes 5] [--cadence 30]

Arms run the stretch values interleaved (4,1 with --arms 2 runs 4 1 1 4), each on a fresh fleet
(fleet.py reset, this user's port block) that is removed after the arm:
  1. settle --settle seconds, then sample each validator container's CPU every 5 s for --idle
     seconds with no transactions (CPU % of one core per container). Every validator's latest and
     finalized heads are read at the window's start and end only, so RPC polling stays out of the
     CPU window: blocks and consensus rounds per second per node (the stretch multiplies the DAG
     round delay) and whether all four latest and finalized heads moved;
  2. --probes times: wait --probe-idle seconds idle, sampling CPU and heads over the gap's quiet part
     (idle after activity) and polling node 1's head every 50 ms over the gap's last --cadence
     seconds (idle block intervals), then submit one signed transfer from the
     dev key to node 1 and time submit->first new block and submit->receipt on node 1, and
     submit->finalized (every validator's `finalized` head passing the receipt block);
  3. per probe, whether each validator logged leaving idle mode when it arrived; at the end, every
     validator's block hash at the lowest finalized head and any validator container that is no
     longer running.
The 1-minute host load is recorded throughout; CPU share is the contention-tolerant measure.
Results: $BENCH_RESULTS_BASE/idle/<stamp>/idle.json, idle.md.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FLEET = ROOT / "bench-targets" / "chains" / "diesis" / "fleet.py"
DEV_KEY = "0x" + "0" * 63 + "1"
SINK = "0x000000000000000000000000000000000000dEaD"


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


observer = load(HERE / "diesis-observer.py", "diesis_observer")
fleet = load(FLEET, "fleet")


def rpc(url, method, params=()):
    return observer.rpc(url, method, params, timeout=10)


def schedule(values, arms):
    """The values interleaved, forward then reversed, until each has `arms` arms: 4,1 x2 -> 4 1 1 4."""
    order, forward = [], True
    while any(order.count(v) < arms for v in values):
        order += values if forward else values[::-1]
        forward = not forward
    return order


def median(values):
    values = [v for v in values if v is not None]
    return statistics.median(values) if values else None


def heads(urls):
    """Every validator's latest and finalized heights, None where its RPC fails."""
    out = dict(at=time.monotonic(), latest=[], finalized=[])
    for url in urls:
        try:
            latest = int(rpc(url, "eth_blockNumber"), 16)
            final = int(rpc(url, "eth_getBlockByNumber", ["finalized", False])["number"], 16)
        except Exception:
            latest = final = None
        out["latest"].append(latest)
        out["finalized"].append(final)
    return out


def advance(start, end):
    """Blocks per second per node between two head readings, and whether every validator's latest
    and finalized heads moved."""
    span = end["at"] - start["at"]

    def rate(key):
        return [None if a is None or b is None else (b - a) / span for a, b in zip(start[key], end[key])]

    moved = all(a is not None and b is not None and b > a
                for key in ("latest", "finalized") for a, b in zip(start[key], end[key]))
    return dict(seconds=span, start=start, end=end, blocks_per_s=rate("latest"), finalized_per_s=rate("finalized"),
                all_advanced=moved)


def rounds(metrics_urls):
    """Every validator's consensus round (reth_diesis_consensus_current_round), None where its
    scrape fails."""
    out = []
    for url in metrics_urls:
        try:
            out.append(observer.gauge(observer.scrape(url), "reth_diesis_consensus_current_round"))
        except Exception:
            out.append(None)
    return out


def per_second(start, end, seconds):
    return [None if a is None or b is None else (b - a) / seconds for a, b in zip(start, end)]


def idle_exits(containers, since):
    """Per validator, the "exiting idle mode" lines (target idle_controller) it logged since `since`
    (Unix seconds). The node logs one only when it was idle, so one per validator right after a probe
    shows every validator was in idle mode when the transaction arrived. Whole-log counts are not
    used: the fleet's container logs rotate at 3 x 10 MB."""
    out = {}
    for name in containers:
        logs = subprocess.run(["docker", "logs", "--since", f"{since:.3f}", name], capture_output=True, text=True,
                              timeout=120)
        out[name] = (logs.stdout + logs.stderr).count("exiting idle mode")
    return out


def gaps_ms(seen):
    """Block intervals in ms from (first-seen time, blocks new at that poll) pairs; a poll that saw
    k new blocks spreads its gap over k."""
    return [(b[0] - a[0]) * 1000 / b[1] for a, b in zip(seen, seen[1:])]


def intervals(url, seconds):
    """Node 1's new-block first-seen times, polled every 50 ms for `seconds`, as intervals in ms."""
    seen, last = [], None
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            head = int(rpc(url, "eth_blockNumber"), 16)
        except Exception:
            head = None
        if head is not None:
            if last is not None and head > last:
                seen.append((time.monotonic(), head - last))
            last = head
        time.sleep(0.05)
    return gaps_ms(seen)


def idle_cpu(containers, seconds, sock):
    """CPU % of one core per container and host load over `seconds` of sampling every 5 s."""
    first = {n: observer.container_stats(n, sock) for n in containers}
    started = time.monotonic()
    loads, peaks = [], {n: 0 for n in containers}
    while time.monotonic() - started < seconds:
        time.sleep(5)
        loads.append(os.getloadavg()[0])
        for n in containers:
            peaks[n] = max(peaks[n], observer.container_stats(n, sock)[1])
    last = {n: observer.container_stats(n, sock) for n in containers}
    span = time.monotonic() - started
    pct = {n: (last[n][0] - first[n][0]) / 1e9 / span * 100 for n in containers}
    return dict(seconds=span, cpu_pct=pct, cpu_pct_median=statistics.median(pct.values()),
                peak_memory_bytes=peaks, host_load_1m=dict(median=statistics.median(loads), max=max(loads)))


def gap_summary(windows):
    """The quiet part of every probe gap (after activity, before the cadence polling): per validator
    the median CPU % over the gaps, blocks per second, and whether every head advanced in every gap."""
    if not windows:
        return None
    cpu = {name: statistics.median(w["cpu_pct"][name] for w in windows) for name in windows[0]["cpu_pct"]}
    return dict(windows=len(windows), seconds=sum(w["seconds"] for w in windows), cpu_pct=cpu,
                cpu_pct_median=statistics.median(cpu.values()),
                blocks_per_s=median([median(w["heads"]["blocks_per_s"]) for w in windows]),
                all_advanced=all(w["heads"]["all_advanced"] for w in windows),
                host_load_1m=dict(median=median([w["host_load_1m"]["median"] for w in windows]),
                                  max=max(w["host_load_1m"]["max"] for w in windows)),
                gaps=windows)


def probe(urls, chain_id):
    """One transfer: submit->first new block and submit->receipt on node 1, and submit->every
    validator's finalized head passing the receipt block, in seconds."""
    sender = subprocess.check_output(["cast", "wallet", "address", "--private-key", DEV_KEY], text=True).strip()
    nonce = int(rpc(urls[0], "eth_getTransactionCount", [sender, "pending"]), 16)
    gas_price = int(rpc(urls[0], "eth_gasPrice"), 16) * 2
    raw = subprocess.check_output(["cast", "mktx", "--private-key", DEV_KEY, "--chain", str(chain_id), "--nonce",
                                   str(nonce), "--gas-price", str(gas_price), "--gas-limit", "21000", "--legacy",
                                   SINK, "--value", "1"], text=True).strip()
    head = int(rpc(urls[0], "eth_blockNumber"), 16)
    started, wall = time.monotonic(), time.time()
    tx = rpc(urls[0], "eth_sendRawTransaction", [raw])
    first_at = receipt_at = block = final_at = None
    errors = []
    deadline = started + 180
    while time.monotonic() < deadline and final_at is None:
        try:
            if first_at is None and int(rpc(urls[0], "eth_blockNumber"), 16) > head:
                first_at = time.monotonic()
            if block is None:
                receipt = rpc(urls[0], "eth_getTransactionReceipt", [tx])
                if receipt:
                    block, receipt_at = int(receipt["blockNumber"], 16), time.monotonic()
            if block is not None:
                finals = [int(rpc(u, "eth_getBlockByNumber", ["finalized", False])["number"], 16) for u in urls]
                if min(finals) >= block:
                    final_at = time.monotonic()
        except Exception as error:
            if len(errors) < 5:
                errors.append(str(error)[:160])
        time.sleep(0.05)

    def since(at):
        return None if at is None else at - started

    return dict(tx=tx, submitted=wall, block=block, first_block_s=since(first_at), receipt_s=since(receipt_at),
                finalized_s=since(final_at), host_load_1m=os.getloadavg()[0], errors=errors)


def agreement(urls):
    """Every validator's block hash at the lowest finalized head."""
    try:
        head = min(int(rpc(u, "eth_getBlockByNumber", ["finalized", False])["number"], 16) for u in urls)
        hashes = {rpc(u, "eth_getBlockByNumber", [hex(head), False])["hash"] for u in urls}
        return dict(height=head, agree=len(hashes) == 1)
    except Exception as error:
        return dict(agree=None, error=str(error)[:200])


def not_running(containers):
    out = subprocess.run(["docker", "inspect", "--format", "{{.Name}} {{.State.Status}} {{.State.ExitCode}}",
                          *containers], capture_output=True, text=True).stdout
    return [line for line in out.splitlines() if line.split()[1:2] != ["running"]]


def spread(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return dict(median=statistics.median(values), min=min(values), max=max(values), n=len(values))


def run_arm(number, value, a, out, sock):
    env = dict(os.environ, BENCH_DIESIS_PROFILE=a.profile, BENCH_DIESIS_ENV=f"DIESIS_IDLE_STRETCH={value}")
    env.pop("DIESIS_AGED_TAG", None)
    arm = dict(arm=number, stretch=int(value))
    try:
        subprocess.run([sys.executable, "-B", str(FLEET), "reset"], env=env, check=True,
                       stdout=(out / f"fleet-{number}-{value}.log").open("w"), stderr=subprocess.STDOUT)
        ports = fleet.project_ports(fleet.fresh_project())
        urls = [f"http://127.0.0.1:{p}" for p in ports["rpc"]]
        chain_id = int(rpc(urls[0], "eth_chainId"), 16)
        provenance = json.loads((fleet.state_dir() / fleet.fresh_project() / "provenance.json").read_text())
        arm.update(block=ports["block"], image=provenance.get("image"), image_id=provenance.get("image_id"),
                   checkout_head=provenance.get("checkout_head"), validator_env=provenance.get("validator_env"),
                   cpusets=provenance.get("cpusets"))
        metrics = [f"http://127.0.0.1:{p}/metrics" for p in ports["metrics"]]
        time.sleep(a.settle)
        start, round_start = heads(urls), rounds(metrics)
        arm["idle"] = idle_cpu(ports["containers"], a.idle, sock)
        arm["idle_heads"] = advance(start, heads(urls))
        arm["idle_rounds_per_s"] = per_second(round_start, rounds(metrics), arm["idle_heads"]["seconds"])
        probes, gaps, quiet = [], [], []
        for _ in range(a.probes):
            rest = max(0, a.probe_idle - a.cadence)
            if rest >= 10:
                before = heads(urls)
                window = idle_cpu(ports["containers"], rest, sock)
                window["heads"] = advance(before, heads(urls))
                quiet.append(window)
            else:
                time.sleep(rest)
            gaps += intervals(urls[0], min(a.cadence, a.probe_idle))
            result = probe(urls, chain_id)
            result["idle_exits"] = idle_exits(ports["containers"], result["submitted"] - 5)
            probes.append(result)
        arm["probes"] = probes
        arm["gap_idle"] = gap_summary(quiet)
        arm["idle_interval_ms"] = observer.spread(gaps, (50, 90, 99))
        for key in ("first_block_s", "receipt_s", "finalized_s"):
            arm[key] = spread([x[key] for x in probes])
        arm["agreement"] = agreement(urls)
        arm["not_running"] = not_running(ports["containers"])
        arm["idle_mode"] = {name: sum(bool(x["idle_exits"].get(name)) for x in probes) for name in ports["containers"]}
        arm["result"] = "completed"
    except Exception as error:
        arm["result"] = f"failed: {error!r}"
    finally:
        subprocess.run([sys.executable, "-B", str(FLEET), "down"], env=env, capture_output=True)
    return arm


def fmt(value, digits=2):
    return "-" if value is None else f"{value:.{digits}f}"


def live(arm):
    agree = arm.get("agreement") or {}
    return (f"{'yes' if arm['idle_heads']['all_advanced'] else 'NO'}; agree {agree.get('agree')}"
            f"{'' if not arm.get('not_running') else '; exited ' + ', '.join(arm['not_running'])}")


def report(record):
    """Markdown: one row per arm, then medians per stretch value over its completed arms."""
    lines = [f"# Idle stretch ({record['profile']}, {record['idle_s']} s idle CPU window, {record['probes']} probes "
             f"after {record['probe_idle_s']} s idle each)", "",
             "CPU is % of one core per validator container (the capped profile allows 200%). Idle blocks/s is the "
             "median over the four validators' latest heads across the CPU window; intervals are node 1's "
             "first-seen block gaps in the probe gaps. Live: all four latest and finalized heads advanced during "
             "the idle window; agree: every validator's hash at the lowest finalized head at the arm's end.", "",
             "| arm | stretch | CPU % per validator | median CPU % | idle blocks/s | idle rounds/s | "
             "idle interval p50/p99 ms | submit→first block s | submit→receipt s | "
             "submit→finalized s (median, min–max) | live | probes leaving idle mode, per validator | "
             "host load median/max |", "|---" * 13 + "|"]
    for arm in record["arms"]:
        if arm.get("result") != "completed":
            lines.append(f"| {arm['arm']} | {arm['stretch']} | {arm.get('result')} |" + " |" * 10)
            continue
        i, f = arm["idle"], arm["finalized_s"] or {}
        per = ", ".join(f"{v:.1f}" for v in i["cpu_pct"].values())
        gap = arm["idle_interval_ms"]
        modes = ", ".join(f"{n}/{len(arm['probes'])}" for n in (arm.get("idle_mode") or {}).values())
        lines.append(f"| {arm['arm']} | {arm['stretch']} | {per} | {i['cpu_pct_median']:.1f} | "
                     f"{fmt(median(arm['idle_heads']['blocks_per_s']))} | {fmt(median(arm.get('idle_rounds_per_s') or []))} | "
                     f"{fmt(gap['p50'], 0)}/{fmt(gap['p99'], 0)} | "
                     f"{fmt((arm['first_block_s'] or {}).get('median'))} | {fmt((arm['receipt_s'] or {}).get('median'))} | "
                     f"{fmt(f.get('median'))} ({fmt(f.get('min'))}–{fmt(f.get('max'))}, n={f.get('n', 0)}) | "
                     f"{live(arm)} | {modes or '-'} | {i['host_load_1m']['median']:.1f}/{i['host_load_1m']['max']:.1f} |")
    lines += ["", "After activity: the quiet part of each probe gap (before the cadence polling), medians over the "
              "gaps. Blank for arms run before this was recorded.", "",
              "| arm | stretch | CPU % per validator | median CPU % | blocks/s | every head advanced | "
              "host load median/max |", "|---" * 7 + "|"]
    for arm in record["arms"]:
        g = arm.get("gap_idle") if arm.get("result") == "completed" else None
        if not g:
            lines.append(f"| {arm['arm']} | {arm['stretch']} | - | - | - | - | - |")
            continue
        per = ", ".join(f"{v:.1f}" for v in g["cpu_pct"].values())
        lines.append(f"| {arm['arm']} | {arm['stretch']} | {per} | {g['cpu_pct_median']:.1f} | {fmt(g['blocks_per_s'])} | "
                     f"{'yes' if g['all_advanced'] else 'NO'} | {fmt(g['host_load_1m']['median'], 1)}/"
                     f"{fmt(g['host_load_1m']['max'], 1)} |")
    lines += ["", "| stretch | arms | median CPU % (min–max validator) | idle blocks/s | idle rounds/s | "
              "idle interval p50/p99 ms | submit→first block s | submit→receipt s | "
              "submit→finalized s (median, min–max) | live arms |", "|---" * 10 + "|"]
    for value in dict.fromkeys(arm["stretch"] for arm in record["arms"]):
        done = [arm for arm in record["arms"] if arm["stretch"] == value and arm.get("result") == "completed"]
        if not done:
            lines.append(f"| {value} | 0 |" + " |" * 8)
            continue
        cpu = [v for arm in done for v in arm["idle"]["cpu_pct"].values()]
        gaps = [arm["idle_interval_ms"] for arm in done]
        probes = [p for arm in done for p in arm["probes"]]
        f = spread([p["finalized_s"] for p in probes]) or {}
        lines.append(f"| {value} | {len(done)} | {median([arm['idle']['cpu_pct_median'] for arm in done]):.1f} "
                     f"({min(cpu):.1f}–{max(cpu):.1f}) | "
                     f"{fmt(median([median(arm['idle_heads']['blocks_per_s']) for arm in done]))} | "
                     f"{fmt(median([median(arm.get('idle_rounds_per_s') or []) for arm in done]))} | "
                     f"{fmt(median([g['p50'] for g in gaps]), 0)}/{fmt(median([g['p99'] for g in gaps]), 0)} | "
                     f"{fmt(median([p['first_block_s'] for p in probes]))} | {fmt(median([p['receipt_s'] for p in probes]))} | "
                     f"{fmt(f.get('median'))} ({fmt(f.get('min'))}–{fmt(f.get('max'))}, n={f.get('n', 0)}) | "
                     f"{sum(arm['idle_heads']['all_advanced'] and (arm.get('agreement') or {}).get('agree') is True and not arm.get('not_running') for arm in done)}/{len(done)} |")
    return "\n".join(lines) + "\n"


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--stretch", default="4,1")
    p.add_argument("--arms", type=int, default=1, help="Arms per stretch value, interleaved")
    p.add_argument("--profile", choices=["capped", "uncapped"], default="capped")
    p.add_argument("--settle", type=int, default=60)
    p.add_argument("--idle", type=int, default=300)
    p.add_argument("--probe-idle", type=int, default=120)
    p.add_argument("--probes", type=int, default=5)
    p.add_argument("--cadence", type=int, default=30, help="Seconds of 50 ms head polling before each probe")
    a = p.parse_args()
    base = Path(os.environ.get("BENCH_RESULTS_BASE", ROOT / "bench-targets" / "results"))
    out = base / "idle" / time.strftime("%Y%m%d-%H%M%S")
    out.mkdir(parents=True)
    sock = observer.docker_socket()
    order = schedule(a.stretch.split(","), a.arms)
    record = dict(profile=a.profile, settle_s=a.settle, idle_s=a.idle, probe_idle_s=a.probe_idle, probes=a.probes,
                  cadence_s=a.cadence, order=[int(v) for v in order], arms=[])
    for number, value in enumerate(order, 1):
        record["arms"].append(run_arm(number, value, a, out, sock))
        (out / "idle.json").write_text(json.dumps(record, indent=2) + "\n")
        (out / "idle.md").write_text(report(record))
    print((out / "idle.md").read_text())


if __name__ == "__main__":
    main()
