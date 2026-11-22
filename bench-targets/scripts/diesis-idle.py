#!/usr/bin/env python3
"""Idle behaviour of a Diesis fleet per DIESIS_IDLE_STRETCH value (docs/diesis-methodology.md).

  diesis-idle.py --stretch 4,1 [--profile capped] [--settle 60] [--idle 300] [--probe-idle 120] [--probes 5]

For each stretch value, in order, on a fresh fleet (fleet.py reset, this user's port block):
  1. settle --settle seconds, then sample each validator container's CPU every 5 s for --idle
     seconds with no transactions, reporting average CPU cores per container;
  2. --probes times: wait --probe-idle seconds idle, then submit one signed transfer from the
     dev key to node 1 and time submit->receipt (node 1) and submit->finalized (the all-four
     `finalized` head passing the receipt block).
The 1-minute host load is recorded throughout; CPU share is the contention-tolerant measure.
The fleet is removed after each value. Results: $BENCH_RESULTS_BASE/idle/<stamp>/idle.json, idle.md.
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


def idle_cpu(containers, seconds, sock):
    """Average CPU cores per container and host load over `seconds` of sampling."""
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
    cores = {n: (last[n][0] - first[n][0]) / 1e9 / span for n in containers}
    return dict(seconds=span, cpu_cores=cores, cpu_cores_median=statistics.median(cores.values()),
                peak_memory_bytes=peaks, host_load_1m=dict(median=statistics.median(loads), max=max(loads)))


def probe(urls, chain_id):
    """One transfer: submit->receipt on node 1 and submit->all-four finalized, in seconds."""
    sender = subprocess.check_output(["cast", "wallet", "address", "--private-key", DEV_KEY], text=True).strip()
    nonce = int(rpc(urls[0], "eth_getTransactionCount", [sender, "pending"]), 16)
    gas_price = int(rpc(urls[0], "eth_gasPrice"), 16) * 2
    raw = subprocess.check_output(["cast", "mktx", "--private-key", DEV_KEY, "--chain", str(chain_id), "--nonce",
                                   str(nonce), "--gas-price", str(gas_price), "--gas-limit", "21000", "--legacy",
                                   SINK, "--value", "1"], text=True).strip()
    started = time.monotonic()
    tx = rpc(urls[0], "eth_sendRawTransaction", [raw])
    receipt_at = block = final_at = None
    deadline = started + 180
    while time.monotonic() < deadline and final_at is None:
        if block is None:
            receipt = rpc(urls[0], "eth_getTransactionReceipt", [tx])
            if receipt:
                block, receipt_at = int(receipt["blockNumber"], 16), time.monotonic()
        if block is not None:
            heads = [int(rpc(u, "eth_getBlockByNumber", ["finalized", False])["number"], 16) for u in urls]
            if min(heads) >= block:
                final_at = time.monotonic()
        time.sleep(0.05)
    return dict(tx=tx, block=block, receipt_s=None if receipt_at is None else receipt_at - started,
                finalized_s=None if final_at is None else final_at - started, host_load_1m=os.getloadavg()[0])


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--stretch", default="4,1")
    p.add_argument("--profile", choices=["capped", "uncapped"], default="capped")
    p.add_argument("--settle", type=int, default=60)
    p.add_argument("--idle", type=int, default=300)
    p.add_argument("--probe-idle", type=int, default=120)
    p.add_argument("--probes", type=int, default=5)
    a = p.parse_args()
    base = Path(os.environ.get("BENCH_RESULTS_BASE", ROOT / "bench-targets" / "results"))
    out = base / "idle" / time.strftime("%Y%m%d-%H%M%S")
    out.mkdir(parents=True)
    sock = observer.docker_socket()
    record = dict(profile=a.profile, settle_s=a.settle, idle_s=a.idle, probe_idle_s=a.probe_idle, arms={})
    for value in a.stretch.split(","):
        env = dict(os.environ, BENCH_DIESIS_PROFILE=a.profile, BENCH_DIESIS_ENV=f"DIESIS_IDLE_STRETCH={value}")
        env.pop("DIESIS_AGED_TAG", None)
        arm = dict(stretch=int(value))
        try:
            subprocess.run([sys.executable, "-B", str(FLEET), "reset"], env=env, check=True,
                           stdout=(out / f"fleet-{value}.log").open("w"), stderr=subprocess.STDOUT)
            ports = fleet.project_ports(fleet.fresh_project())
            urls = [f"http://127.0.0.1:{p}" for p in ports["rpc"]]
            chain_id = int(rpc(urls[0], "eth_chainId"), 16)
            arm.update(block=ports["block"], provenance=json.loads(
                (fleet.state_dir() / fleet.fresh_project() / "provenance.json").read_text()).get("validator_env"))
            time.sleep(a.settle)
            arm["idle"] = idle_cpu(ports["containers"], a.idle, sock)
            probes = []
            for _ in range(a.probes):
                time.sleep(a.probe_idle)
                probes.append(probe(urls, chain_id))
            arm["probes"] = probes
            done = [x["finalized_s"] for x in probes if x["finalized_s"] is not None]
            arm["finalized_s"] = dict(median=statistics.median(done), min=min(done), max=max(done),
                                      n=len(done)) if done else None
            receipts = [x["receipt_s"] for x in probes if x["receipt_s"] is not None]
            arm["receipt_s"] = dict(median=statistics.median(receipts), min=min(receipts), max=max(receipts),
                                    n=len(receipts)) if receipts else None
            arm["result"] = "completed"
        except Exception as error:
            arm["result"] = f"failed: {error!r}"
        finally:
            subprocess.run([sys.executable, "-B", str(FLEET), "down"], env=env, capture_output=True)
        record["arms"][value] = arm
        (out / "idle.json").write_text(json.dumps(record, indent=2) + "\n")
    lines = [f"# Idle stretch ({a.profile}, {a.idle} s idle CPU, {a.probes} probes after {a.probe_idle} s idle)", "",
             "| stretch | median CPU cores/validator | per validator | probe submit→finalized s (median, min–max) | "
             "submit→receipt s (median) | host load during idle (median/max) |", "|---|---|---|---|---|---|"]
    for value, arm in record["arms"].items():
        if arm.get("result") != "completed":
            lines.append(f"| {value} | {arm.get('result')} | | | | |")
            continue
        i, f, r = arm["idle"], arm["finalized_s"] or {}, arm["receipt_s"] or {}
        per = ", ".join(f"{v:.2f}" for v in i["cpu_cores"].values())
        lines.append(f"| {value} | {i['cpu_cores_median']:.2f} | {per} | {f.get('median', float('nan')):.2f} "
                     f"({f.get('min', float('nan')):.2f}–{f.get('max', float('nan')):.2f}, n={f.get('n', 0)}) | "
                     f"{r.get('median', float('nan')):.2f} | {i['host_load_1m']['median']:.1f}/{i['host_load_1m']['max']:.1f} |")
    (out / "idle.md").write_text("\n".join(lines) + "\n")
    print((out / "idle.md").read_text())


if __name__ == "__main__":
    main()
