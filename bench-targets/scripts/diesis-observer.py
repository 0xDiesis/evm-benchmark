#!/usr/bin/env python3
"""Independent chain-side observer for Diesis benchmark runs (docs/diesis-methodology.md).

bench.sh starts it before the harness and sends SIGUSR1 when the load ends. It samples:
  - node 1's block trace with transaction hashes (50 ms poll),
  - the all-four `finalized` head and the all-four latest height (100 ms),
  - each validator's Prometheus subset and Docker CPU counter (5 s),
  - validator exits (5 s).
After SIGUSR1 it waits up to --drain seconds for the window's blocks to finalize and the
pool to empty, then joins the harness submit log (BENCH_SUBMIT_LOG) and report.json and
writes <run-dir>/diesis-metrics.json.

The measured window is [load start + --warmup, load start + --warmup + --window], with load
start = the first submission in the submit log (wall clock, same host). Primary metrics:
finalized TPS, submit->finalized and submit->receipt p50/p95/p99 over every transaction
submitted inside the window (one never included or finalized by the end of the drain counts
as infinite, so a percentile that lands on it is null and `unfinalized_share` says how many).
Secondary: latest->finalized, block interval, rounds/block, selected-owner hold and MPT
quorum wait, CPU-seconds per transaction, pool backlog slope, empty-block share, and the
generator_bound / chain_saturated verdicts.

  diesis-observer.py watch --run-dir DIR --target-tps N --duration S [--warmup W] [--drain D]
  diesis-observer.py summary --metrics DIR/diesis-metrics.json
"""
import argparse
import http.client
import json
import os
from pathlib import Path
import re
import signal
import socket
import statistics
import subprocess
import threading
import time
from urllib.request import Request, urlopen

# Endpoints default to the fleet.py / e2e layout; an external fleet (DIESIS_FLEET=external)
# passes its own. Container CPU is read only for containers that exist on this host.
RPC_URLS = os.environ.get("DIESIS_RPC", "http://127.0.0.1:18545,http://127.0.0.1:18555,"
                          "http://127.0.0.1:18565,http://127.0.0.1:18575").split(",")
METRICS_URLS = os.environ.get("DIESIS_METRICS", ",".join(f"http://127.0.0.1:{p}/metrics"
                                                          for p in (19101, 19102, 19103, 19104))).split(",")
CONTAINERS = [c for c in os.environ.get("DIESIS_CONTAINERS", ",".join(f"diesis-e2e-node-{i}" for i in range(1, 5)))
              .split(",") if c]
PROM = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{[^}]*\})?\s+(\S+)$")
KEEP = ("reth_sync_checkpoint", "reth_diesis_horizon_committed_transactions_total",
        "reth_diesis_consensus_current_round", "reth_diesis_lock_hold_ms_sum", "reth_diesis_mpt_certify_phase_ms",
        "reth_diesis_mpt_certify_phase_ms_sum", "reth_transaction_pool_pending_pool_transactions")
INF = float("inf")


def rpc(url, method, params=(), timeout=5):
    body = json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=list(params))).encode()
    with urlopen(Request(url, body, {"Content-Type": "application/json"}),
                 timeout=timeout) as response:
        data = json.load(response)
    if "error" in data:
        raise RuntimeError(f"{method}: {data['error']}")
    return data["result"]


def parse_prom(text, keep=KEEP):
    out = {}
    for line in text.splitlines():
        if line.startswith("#") or "_bucket{" in line:
            continue
        m = PROM.match(line.strip())
        if m and m.group(1) in keep:
            out[(m.group(1), m.group(2) or "")] = float(m.group(3))
    return out


def scrape(url):
    with urlopen(url, timeout=10) as response:
        return parse_prom(response.read().decode())


def delta(s, e, name, flt=""):
    return sum(v - s.get(k, 0) for k, v in e.items() if k[0] == name and flt in k[1] and 'quantile="' not in k[1])


def gauge(p, name):
    return sum(v for k, v in p.items() if k[0] == name and 'quantile="' not in k[1])


def per_block(s, e, seconds):
    blocks = delta(s, e, "reth_sync_checkpoint", 'stage="Finish"')
    if not blocks:
        return None
    return dict(blocks=blocks,
                tx_per_block=delta(s, e, "reth_diesis_horizon_committed_transactions_total") / blocks,
                interval_ms=1000 * seconds / blocks,
                rounds_per_block=delta(s, e, "reth_diesis_consensus_current_round") / blocks,
                hold_ms_per_block=delta(s, e, "reth_diesis_lock_hold_ms_sum", 'lock="selected_owner"') / blocks,
                quorum_wait_ms_per_block=delta(s, e, "reth_diesis_mpt_certify_phase_ms_sum",
                                               'phase="quorum_wait"') / blocks)


def quantile(p, q):
    return next((v for (name, labels), v in p.items() if name == "reth_diesis_mpt_certify_phase_ms"
                 and 'phase="quorum_wait"' in labels and f'quantile="{q}"' in labels), None)


def pct(values, p):
    ordered = sorted(values)
    if not ordered:
        return None
    value = ordered[max(0, -(-len(ordered) * p // 100) - 1)]
    return None if value == INF else value


def spread(values, ps=(50, 95, 99)):
    return {f"p{p}": pct(values, p) for p in ps} | dict(n=len(values))


def slope(points):
    if len(points) < 3:
        return None
    mx = statistics.fmean(x for x, _ in points)
    my = statistics.fmean(y for _, y in points)
    sxx = sum((x - mx) ** 2 for x, _ in points)
    return sum((x - mx) * (y - my) for x, y in points) / sxx if sxx else None


class DockerConnection(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("localhost", timeout=10)
        self.path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def docker_socket():
    context = json.loads(subprocess.check_output(["docker", "context", "inspect"], text=True))[0]
    return context["Endpoints"]["docker"]["Host"].removeprefix("unix://")


def cpu_ns(name, sock):
    conn = DockerConnection(sock)
    try:
        conn.request("GET", f"/{os.environ.get('DIESIS_DOCKER_API', 'v1.47')}/containers/{name}/stats"
                            "?stream=false&one-shot=true")
        response = conn.getresponse()
        if response.status != 200:
            raise RuntimeError(f"docker stats {response.status}")
        return json.load(response)["cpu_stats"]["cpu_usage"]["total_usage"]
    finally:
        conn.close()


class Observer:
    def __init__(self):
        self.lock = threading.Lock()
        self.blocks = {}      # number -> seen, hashes, finalized, all4 (wall clock)
        self.samples = []     # (wall, [per-node parsed], {container: cpu_ns}, load1)
        self.exited = []
        self.errors = []
        self.stop = threading.Event()
        self.sock = docker_socket()
        present = subprocess.run(["docker", "ps", "--format", "{{.Names}}"], capture_output=True, text=True).stdout.split()
        self.containers = [c for c in CONTAINERS if c in present]
        if not self.containers:
            self.err("no local validator containers: CPU per transaction and exit detection are off")

    def err(self, text):
        if len(self.errors) < 40:
            self.errors.append(f"{time.time():.1f} {text}"[:300])

    def blocks_loop(self):
        last = int(rpc(RPC_URLS[0], "eth_blockNumber"), 16)
        while not self.stop.is_set():
            try:
                head = int(rpc(RPC_URLS[0], "eth_blockNumber"), 16)
                seen = time.time()
                for number in range(last + 1, head + 1):
                    block = rpc(RPC_URLS[0], "eth_getBlockByNumber", [hex(number), False], 10)
                    with self.lock:
                        self.blocks.setdefault(number, {}).update(seen=seen, hashes=block["transactions"])
                    last = number
            except Exception as error:
                self.err(f"blocks: {error}")
            self.stop.wait(0.05)

    def heads_loop(self):
        floor = all4 = int(rpc(RPC_URLS[0], "eth_blockNumber"), 16)
        while not self.stop.is_set():
            try:
                finalized = min(int(rpc(p, "eth_getBlockByNumber", ["finalized", False])["number"], 16) for p in RPC_URLS)
                latest = min(int(rpc(p, "eth_blockNumber"), 16) for p in RPC_URLS)
                seen = time.time()
                with self.lock:
                    for number in range(floor + 1, finalized + 1):
                        self.blocks.setdefault(number, {})["finalized"] = seen
                    for number in range(all4 + 1, latest + 1):
                        self.blocks.setdefault(number, {})["all4"] = seen
                floor, all4 = max(floor, finalized), max(all4, latest)
            except Exception as error:
                self.err(f"heads: {error}")
            self.stop.wait(0.1)

    def resources_loop(self):
        while not self.stop.is_set():
            wall = time.time()
            nodes, cpu = [], {}
            for url in METRICS_URLS:
                try:
                    nodes.append(scrape(url))
                except Exception as error:
                    nodes.append(None)
                    self.err(f"prom {url}: {error}")
            for name in self.containers:
                try:
                    cpu[name] = cpu_ns(name, self.sock)
                except Exception as error:
                    self.err(f"cpu {name}: {error}")
            self.samples.append((wall, nodes, cpu, os.getloadavg()[0]))
            try:
                if not self.containers:
                    self.stop.wait(5)
                    continue
                state = subprocess.run(["docker", "inspect", "--format", "{{.Name}} {{.State.Status}} {{.State.ExitCode}}",
                                        *self.containers], capture_output=True, text=True, timeout=10).stdout
                gone = [line for line in state.splitlines() if line.split()[1:2] in (["exited"], ["dead"])]
                if gone:
                    self.exited = gone
            except Exception as error:
                self.err(f"exits: {error}")
            self.stop.wait(5)

    def run(self):
        threads = [threading.Thread(target=f, daemon=True) for f in (self.blocks_loop, self.heads_loop,
                                                                     self.resources_loop)]
        for t in threads:
            t.start()
        return threads


def window_metrics(obs, submit_log, report, target_tps, duration, warmup, pool_cap):
    """Compute every metric for one run; pure over the observer's recorded state."""
    rows = []
    with open(submit_log) as f:
        next(f, None)
        for line in f:
            h, us = line.rstrip().split(",")
            rows.append((h, int(us) / 1e6))
    if not rows:
        raise SystemExit("Empty submit log")
    load_start = min(t for _, t in rows)
    start, end = load_start + warmup, load_start + duration
    window = end - start
    with obs.lock:
        blocks = {n: dict(b) for n, b in obs.blocks.items() if "seen" in b}
    r = dict(window_start=start, window_end=end, window_s=window, warmup_s=warmup, target_tps=target_tps)
    results = report["results"]
    submit_s = (results.get("submit_ms") or duration * 1000) / 1000
    attempted = results.get("attempted", results.get("submitted", 0))
    accepted = results.get("accepted", results.get("submitted", 0))
    failed = results.get("failed", max(0, attempted - accepted))
    r.update(attempted=attempted, accepted=accepted, rejected=failed,
             rejection_reasons=(results.get("submission_errors") or [])[:5],
             attempted_tps=attempted / submit_s, delivered_tps=accepted / submit_s)
    in_window = [b for b in blocks.values() if start <= b["seen"] <= end]
    txs = lambda b: len(b.get("hashes") or ())
    r["finalized_tps"] = sum(txs(b) for b in blocks.values() if start <= b.get("finalized", -1) <= end) / window
    r["committed_tps"] = sum(txs(b) for b in in_window) / window
    r["all_four_latest_tps"] = sum(txs(b) for b in blocks.values() if start <= b.get("all4", -1) <= end) / window
    where = {h: b for b in blocks.values() for h in (b.get("hashes") or ())}
    receipt, final, no_receipt, no_final = [], [], 0, 0
    for h, sub in rows:
        if not start <= sub <= end:
            continue
        b = where.get(h)
        if b is None:
            no_receipt += 1
            no_final += 1
            receipt.append(INF)
            final.append(INF)
            continue
        receipt.append(b["seen"] - sub)
        if "finalized" in b:
            final.append(b["finalized"] - sub)
        else:
            no_final += 1
            final.append(INF)
    n = len(final)
    r["submit_to_finalized_s"] = spread(final)
    r["submit_to_receipt_s"] = spread(receipt)
    r["unfinalized_share"] = no_final / n if n else None
    r["unreceipted_share"] = no_receipt / n if n else None
    lag = [b["finalized"] - b["seen"] for b in in_window if "finalized" in b for _ in range(txs(b))]
    r["latest_to_finalized_s"] = spread(lag)
    ordered = sorted(in_window, key=lambda b: b["seen"])
    intervals = [(y["seen"] - x["seen"]) * 1000 for x, y in zip(ordered, ordered[1:])]
    r["block_interval_ms"] = spread(intervals, (50, 99)) | dict(mean=window * 1000 / len(in_window) if in_window else None)
    r["empty_block_share"] = sum(1 for b in in_window if not txs(b)) / len(in_window) if in_window else None
    # Resource samples nearest the window bounds; per-node per-block deltas.
    samples = [s for s in obs.samples if all(x is not None for x in s[1])]
    inner = [s for s in samples if start - 2.5 <= s[0] <= end + 2.5]
    nodes = []
    if len(inner) >= 2:
        first, last = inner[0], inner[-1]
        for i in range(4):
            row = per_block(first[1][i], last[1][i], last[0] - first[0])
            if row:
                nodes.append(row)
        cpu = {}
        finalized_txs = r["finalized_tps"] * window
        for name, ns in last[2].items():
            if name in first[2] and finalized_txs:
                cpu[name] = (ns - first[2][name]) / 1e9 / finalized_txs
        r["cpu_s_per_tx_nodes"] = cpu
        r["cpu_s_per_tx_median"] = statistics.median(cpu.values()) if cpu else None
        r["bounds_s"] = [first[0] - start, last[0] - end]
    r["per_block_nodes"] = nodes
    r["per_block_median"] = {k: statistics.median(x[k] for x in nodes) for k in nodes[0]} if nodes else None
    hold, quorum = [], []
    for a, b in zip(inner, inner[1:]):
        row = per_block(a[1][0], b[1][0], b[0] - a[0])
        if row and row["blocks"] >= 2:
            hold.append(row["hold_ms_per_block"])
            quorum.append(row["quorum_wait_ms_per_block"])
    r["hold_ms_per_block_5s"] = spread(hold)
    r["quorum_wait_ms_per_block_5s"] = spread(quorum)
    q = {k: [v for s in inner if (v := quantile(s[1][0], k)) is not None] for k in ("0.5", "0.95", "0.99")}
    r["quorum_wait_ms_exporter"] = {k: statistics.median(v) for k, v in q.items() if v}
    pool = [(s[0] - start, gauge(s[1][0], "reth_transaction_pool_pending_pool_transactions")) for s in inner]
    full_at = next((t for t, v in pool if v >= 0.98 * pool_cap), None)
    r["pool_pending_start_end"] = [pool[0][1], pool[-1][1]] if pool else None
    r["pool_slope_tx_per_s"] = slope([(t, v) for t, v in pool if full_at is None or t < full_at])
    r["pool_full_at_s"] = full_at
    loads = [s[3] for s in inner]
    r["host_load_1m"] = dict(median=statistics.median(loads), max=max(loads)) if loads else None
    r["generator_bound"] = r["attempted_tps"] < 0.95 * target_tps
    r["chain_saturated"] = (r["finalized_tps"] < 0.95 * r["attempted_tps"] or failed > 0.001 * max(attempted, 1)
                            or full_at is not None)
    r["validator_exits"] = obs.exited
    r["observer_errors"] = obs.errors
    return r


def agreement(obs, start, end):
    """All validators' block hashes at up to five finalized heights: the window's first, middle
    and last finalized blocks and the final all-four finalized head."""
    try:
        head = min(int(rpc(u, "eth_getBlockByNumber", ["finalized", False])["number"], 16) for u in RPC_URLS)
    except Exception as error:
        return dict(agree=None, error=str(error)[:200])
    with obs.lock:
        window = sorted(n for n, b in obs.blocks.items() if start <= b.get("finalized", -1) <= end and n <= head)
    heights = sorted({h for h in (window[:1] + window[len(window) // 2:len(window) // 2 + 1] + window[-1:] + [head])})
    checked = []
    for h in heights:
        hashes = []
        for u in RPC_URLS:
            try:
                hashes.append(rpc(u, "eth_getBlockByNumber", [hex(h), False])["hash"])
            except Exception as error:
                hashes.append(f"error: {str(error)[:80]}")
        checked.append(dict(height=h, agree=len(set(hashes)) == 1, hashes=sorted(set(hashes))))
    return dict(agree=all(c["agree"] for c in checked), finalized_head=head, heights=checked)


def watch(a):
    obs = Observer()
    done = threading.Event()
    signal.signal(signal.SIGUSR1, lambda *_: done.set())
    signal.signal(signal.SIGTERM, lambda *_: done.set())
    obs.run()
    (a.run_dir / "observer.pid").write_text(str(os.getpid()))
    done.wait()
    load_end = time.time()
    with obs.lock:
        end_height = max(obs.blocks) if obs.blocks else 0
    deadline = time.monotonic() + a.drain
    while time.monotonic() < deadline:
        with obs.lock:
            pending = [n for n, b in obs.blocks.items() if n <= end_height and "finalized" not in b]
        pool = None
        try:
            pool = gauge(scrape(METRICS_URLS[0]), "reth_transaction_pool_pending_pool_transactions")
        except Exception:
            pass
        if not pending and pool == 0:
            break
        time.sleep(2)
    report = json.loads((a.run_dir / "report.json").read_text())
    metrics = window_metrics(obs, a.run_dir / "submit-log.csv", report, a.target_tps, a.duration, a.warmup,
                             a.pool_cap)
    metrics["drain_s"] = time.time() - load_end
    metrics["finalized_agreement"] = agreement(obs, metrics["window_start"], metrics["window_end"])
    metrics["pool_pending_after_drain"] = pool
    obs.stop.set()
    (a.run_dir / "diesis-metrics.json").write_text(json.dumps(metrics, indent=2) + "\n")
    if not a.keep_raw:
        (a.run_dir / "submit-log.csv").unlink(missing_ok=True)
    print_summary(metrics)


def fmt(v, unit=""):
    return "-" if v is None else (f"{v:.2f}{unit}" if isinstance(v, float) else f"{v}{unit}")


def print_summary(m):
    f, rc = m["submit_to_finalized_s"], m["submit_to_receipt_s"]
    pb = m.get("per_block_median") or {}
    print("--- Diesis chain-side metrics (window "
          f"{m['window_s']:.0f} s after {m['warmup_s']:.0f} s warmup) ---")
    print(f"  Finalized TPS:          {m['finalized_tps']:.1f}  (attempted {m['attempted_tps']:.1f}, "
          f"target {m['target_tps']})")
    print(f"  Submit->finalized s:    p50 {fmt(f['p50'])}  p95 {fmt(f['p95'])}  p99 {fmt(f['p99'])}  "
          f"(unfinalized {fmt(m['unfinalized_share'])})")
    print(f"  Submit->receipt s:      p50 {fmt(rc['p50'])}  p95 {fmt(rc['p95'])}  p99 {fmt(rc['p99'])}")
    print(f"  Latest->finalized s:    p50 {fmt(m['latest_to_finalized_s']['p50'])}  "
          f"p99 {fmt(m['latest_to_finalized_s']['p99'])}")
    print(f"  Block interval ms:      p50 {fmt(m['block_interval_ms']['p50'])}  p99 {fmt(m['block_interval_ms']['p99'])}")
    print(f"  Per block (median):     tx {fmt(pb.get('tx_per_block'))}  rounds {fmt(pb.get('rounds_per_block'))}  "
          f"hold {fmt(pb.get('hold_ms_per_block'))} ms  quorum {fmt(pb.get('quorum_wait_ms_per_block'))} ms")
    print(f"  CPU ms/tx (median node): {fmt((m.get('cpu_s_per_tx_median') or 0) * 1000 or None)}  "
          f"empty blocks {fmt(m['empty_block_share'])}  pool slope {fmt(m['pool_slope_tx_per_s'])}/s")
    print(f"  Verdicts:               generator_bound={m['generator_bound']}  chain_saturated={m['chain_saturated']}"
          + (f"  EXITS={m['validator_exits']}" if m["validator_exits"] else ""))
    agree = m.get("finalized_agreement") or {}
    print(f"  Finalized agreement:    {agree.get('agree')} at heights "
          f"{[c['height'] for c in agree.get('heights', [])]}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("watch")
    w.add_argument("--run-dir", type=Path, required=True)
    w.add_argument("--target-tps", type=float, required=True)
    w.add_argument("--duration", type=float, required=True, help="Load seconds (warmup + window)")
    w.add_argument("--warmup", type=float, default=0)
    w.add_argument("--drain", type=float, default=150)
    w.add_argument("--pool-cap", type=int, default=50000)
    w.add_argument("--keep-raw", action="store_true", help="Keep submit-log.csv")
    s = sub.add_parser("summary")
    s.add_argument("--metrics", type=Path, required=True)
    a = p.parse_args()
    if a.cmd == "watch":
        watch(a)
    else:
        print_summary(json.loads(a.metrics.read_text()))


if __name__ == "__main__":
    main()
