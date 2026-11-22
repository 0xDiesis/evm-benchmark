#!/usr/bin/env python3
"""Independent chain-side observer for Diesis benchmark runs (docs/diesis-methodology.md).

bench.sh starts it before the harness and sends SIGUSR1 when the load ends. It samples:
  - node 1's block trace with transaction hashes (50 ms poll),
  - the all-four `finalized` head and the all-four latest height (100 ms),
  - each validator's Prometheus subset, Docker CPU and CFS throttling counters (5 s),
  - every validator's latest height, and each follower's when DIESIS_FOLLOWER_RPC is set (100 ms),
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
quorum wait, CPU-seconds per transaction and per round, CFS throttled time, pool backlog slope,
empty-block share, owner hold by site, duplicate proposals, output deferrals and fallback entries
per block, validator head spread, follower lag, reverted receipts, and the generator_bound /
chain_saturated verdicts. Round- and vote-timeline log lines (DIESIS_TRACE_ROUND_TIMELINE,
DIESIS_TRACE_TX_SAMPLE, DIESIS_TRACE_MPT_VOTE_TIMELINE) are copied to <run-dir>/timeline-<node>.log.

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
FOLLOWER_URLS = [u for u in os.environ.get("DIESIS_FOLLOWER_RPC", "").split(",") if u]
CONTAINERS = [c for c in os.environ.get("DIESIS_CONTAINERS", ",".join(f"diesis-e2e-node-{i}" for i in range(1, 5)))
              .split(",") if c]
PROM = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{[^}]*\})?\s+(\S+)$")
KEEP = ("reth_sync_checkpoint", "reth_diesis_horizon_committed_transactions_total",
        "reth_diesis_consensus_current_round", "reth_diesis_lock_hold_ms_sum", "reth_diesis_mpt_certify_phase_ms",
        "reth_diesis_mpt_certify_phase_ms_sum", "reth_transaction_pool_pending_pool_transactions",
        "reth_diesis_commit_proposal_duplicate_skipped_total", "reth_diesis_producer_output_deferred_transactions_total",
        "reth_diesis_outcome_fallback_entries_total", "reth_diesis_outcome_fallback_resolved_total",
        "reth_diesis_horizon_flow_held_ms_total", "reth_diesis_tx_source_selected_primary_total",
        "reth_diesis_tx_source_selected_fallback_total", "reth_diesis_tx_source_fallback_owner_live_total",
        "reth_diesis_checkpoint_control_inbound_rejected_total")
TIMELINE_TARGETS = ("diesis_round_timeline", "diesis_mpt_vote_timeline")
INF = float("inf")
# Spot-book trade fills (ExchangeTradeFillV2): topics[2] is the maker order id.
SPOT_BOOK = "0xD1E515000000000000000000000000000000590D"
TRADE_FILL_TOPIC = "0x74eea602401da7376b8e69a8401c0164dea612f7370c6d2ed8f016e86b19e5e2"


def rpc(url, method, params=(), timeout=5):
    body = json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=list(params))).encode()
    with urlopen(Request(url, body, {"Content-Type": "application/json"}),
                 timeout=timeout) as response:
        data = json.load(response)
    if "error" in data:
        raise RuntimeError(f"{method}: {data['error']}")
    return data["result"]


def parse_receipts(raw):
    """hash -> status (1/0), gas used and the spot-book maker order ids it filled, from one block's
    eth_getBlockReceipts result."""
    out = {}
    for r in raw or ():
        makers = [l["topics"][2] for l in r.get("logs") or ()
                  if l.get("address", "").lower() == SPOT_BOOK.lower() and len(l.get("topics") or ()) > 2
                  and l["topics"][0].lower() == TRADE_FILL_TOPIC]
        out[r["transactionHash"].lower()] = dict(status=int(r["status"], 16), gas=int(r["gasUsed"], 16),
                                                 makers=makers)
    return out


def fetch_receipts(numbers, errors, workers=8):
    """Receipts for the given blocks from node 1, read after the drain so the window carries no
    extra RPC load. A block whose receipts cannot be read leaves its hashes status-unknown."""
    from concurrent.futures import ThreadPoolExecutor

    def one(n):
        delay = 0.05
        for attempt in range(10):
            try:
                return parse_receipts(rpc(RPC_URLS[0], "eth_getBlockReceipts", [hex(n)], 30))
            except Exception as error:
                # The node refuses reads past its concurrency limit (-32005); back off and retry.
                if "limit exceeded" in str(error) and attempt < 9:
                    time.sleep(delay)
                    delay = min(delay * 2, 2.0)
                    continue
                errors.append(f"receipts {n}: {str(error)[:120]}")
                return {}

    receipts = {}
    with ThreadPoolExecutor(workers) as pool:
        for part in pool.map(one, sorted(numbers)):
            receipts.update(part)
    return receipts


def read_submit_log(path):
    """(hash, submit wall seconds, method) rows; logs written before the method column read as
    method "unknown"."""
    rows = []
    with open(path) as f:
        next(f, None)
        for line in f:
            parts = line.rstrip().split(",")
            rows.append((parts[0].lower(), int(parts[1]) / 1e6, parts[2] if len(parts) > 2 else "unknown"))
    return rows


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


def by_label(s, e, name, label, flt=""):
    """End-minus-start of every series of `name` whose labels contain `flt`, keyed by the value of
    `label`."""
    out = {}
    for (metric, labels), value in e.items():
        if metric != name or 'quantile="' in labels or flt not in labels:
            continue
        m = re.search(label + r'="([^"]*)"', labels)
        key = m.group(1) if m else ""
        out[key] = out.get(key, 0) + value - s.get((metric, labels), 0)
    return out


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
                                               'phase="quorum_wait"') / blocks,
                duplicate_proposals_per_block=delta(s, e, "reth_diesis_commit_proposal_duplicate_skipped_total") / blocks,
                deferred_tx_per_block=delta(s, e, "reth_diesis_producer_output_deferred_transactions_total") / blocks,
                fallback_entries=delta(s, e, "reth_diesis_outcome_fallback_entries_total"),
                rounds=delta(s, e, "reth_diesis_consensus_current_round"))


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


def container_stats(name, sock):
    """(CPU nanoseconds, working-set bytes: usage minus inactive file cache, CFS throttling as
    (throttled nanoseconds, throttled periods, periods)) for one container."""
    conn = DockerConnection(sock)
    try:
        conn.request("GET", f"/{os.environ.get('DIESIS_DOCKER_API', 'v1.47')}/containers/{name}/stats"
                            "?stream=false&one-shot=true")
        response = conn.getresponse()
        if response.status != 200:
            raise RuntimeError(f"docker stats {response.status}")
        data = json.load(response)
    finally:
        conn.close()
    memory = data.get("memory_stats", {})
    stats = memory.get("stats", {})
    inactive = stats.get("inactive_file", stats.get("total_inactive_file", 0))
    throttling = data["cpu_stats"].get("throttling_data") or {}
    return (data["cpu_stats"]["cpu_usage"]["total_usage"], max(0, memory.get("usage", 0) - inactive),
            (throttling.get("throttled_time", 0), throttling.get("throttled_periods", 0), throttling.get("periods", 0)))


class Observer:
    def __init__(self):
        self.lock = threading.Lock()
        self.blocks = {}      # number -> seen, hashes, finalized, all4 (wall clock)
        self.samples = []     # (wall, [per-node parsed], {container: cpu_ns}, load1, memory, throttling)
        self.heads = []       # (wall, [validator latest], [follower latest or None])
        self.exited = []
        self.errors = []
        self.stop = threading.Event()
        self.captured = set()
        self.run_dir = None
        self.started = time.time()
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
                heads = [int(rpc(p, "eth_blockNumber"), 16) for p in RPC_URLS]
                latest = min(heads)
                followers = []
                for url in FOLLOWER_URLS:
                    try:
                        followers.append(int(rpc(url, "eth_blockNumber"), 16))
                    except Exception:
                        followers.append(None)
                seen = time.time()
                with self.lock:
                    for number in range(floor + 1, finalized + 1):
                        self.blocks.setdefault(number, {})["finalized"] = seen
                    for number in range(all4 + 1, latest + 1):
                        self.blocks.setdefault(number, {})["all4"] = seen
                    self.heads.append((seen, heads, followers))
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
            memory, throttling = {}, {}
            for name in self.containers:
                try:
                    cpu[name], memory[name], throttling[name] = container_stats(name, self.sock)
                except Exception as error:
                    self.err(f"cpu {name}: {error}")
            self.samples.append((wall, nodes, cpu, os.getloadavg()[0], memory, throttling))
            try:
                if not self.containers:
                    self.stop.wait(5)
                    continue
                state = subprocess.run(["docker", "inspect", "--format", "{{.Name}} {{.State.Status}} {{.State.ExitCode}}",
                                        *self.containers], capture_output=True, text=True, timeout=10).stdout
                gone = [line for line in state.splitlines() if line.split()[1:2] in (["exited"], ["dead"])]
                if gone:
                    self.exited = gone
                    self.capture_exit_logs(gone)
            except Exception as error:
                self.err(f"exits: {error}")
            self.stop.wait(5)

    def capture_exit_logs(self, gone):
        """Keep each exited validator's log tail and state the first time it is seen exited,
        before any teardown removes the container."""
        for line in gone:
            name = line.split()[0].lstrip("/")
            if name in self.captured or self.run_dir is None:
                continue
            self.captured.add(name)
            logs = subprocess.run(["docker", "logs", "--tail", "500", name], capture_output=True, text=True, timeout=60)
            (self.run_dir / f"exit-{name}.log").write_text(logs.stdout + logs.stderr)
            state = subprocess.run(["docker", "inspect", "--format", "{{json .State}}", name], capture_output=True,
                                   text=True, timeout=30)
            (self.run_dir / f"exit-{name}.state.json").write_text(state.stdout)

    def run(self):
        threads = [threading.Thread(target=f, daemon=True) for f in (self.blocks_loop, self.heads_loop,
                                                                     self.resources_loop)]
        for t in threads:
            t.start()
        return threads


def window_metrics(obs, submit_log, report, target_tps, duration, warmup, pool_cap, receipts=None):
    """Compute every metric for one run; pure over the observer's recorded state.

    With `receipts` (hash -> status, gas, makers), finalized TPS counts successful receipts only and
    reverted transactions leave the latency populations (reported as `reverted_share`); without
    them, the included basis applies. Per-type results key on the submit log's method column."""
    rows = read_submit_log(submit_log)
    if not rows:
        raise SystemExit("Empty submit log")
    load_start = min(t for _, t, _ in rows)
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
    receipts = receipts or {}
    status = lambda h: receipts.get(h.lower(), {}).get("status")
    finalized_hashes = [h.lower() for b in blocks.values() if start <= b.get("finalized", -1) <= end
                        for h in (b.get("hashes") or ())]
    reverted_n = sum(1 for h in finalized_hashes if status(h) == 0)
    unknown_n = sum(1 for h in finalized_hashes if status(h) is None)
    r["finalized_included_tps"] = len(finalized_hashes) / window
    if receipts:
        r["finalized_basis"] = "successful"
        r["finalized_tps"] = (len(finalized_hashes) - reverted_n - unknown_n) / window
    else:
        r["finalized_basis"] = "included"
        r["finalized_tps"] = r["finalized_included_tps"]
    r["outcomes"] = dict(finalized_successful=len(finalized_hashes) - reverted_n - unknown_n if receipts else None,
                         finalized_reverted=reverted_n if receipts else None,
                         finalized_status_unknown=unknown_n if receipts else None,
                         receipts_read=len(receipts))
    r["committed_tps"] = sum(txs(b) for b in in_window) / window
    r["all_four_latest_tps"] = sum(txs(b) for b in blocks.values() if start <= b.get("all4", -1) <= end) / window
    where = {h.lower(): b for b in blocks.values() for h in (b.get("hashes") or ())}
    receipt, final, no_receipt, no_final, reverted_window = [], [], 0, 0, 0
    per_type = {}
    for h, sub, method in rows:
        if not start <= sub <= end:
            continue
        t = per_type.setdefault(method, dict(submitted_in_window=0, reverted_in_window=0, receipt=[], final=[]))
        t["submitted_in_window"] += 1
        b = where.get(h)
        if b is not None and status(h) == 0:
            reverted_window += 1
            t["reverted_in_window"] += 1
            continue
        if b is None:
            no_receipt += 1
            no_final += 1
            for target in (receipt, final, t["receipt"], t["final"]):
                target.append(INF)
            continue
        receipt.append(b["seen"] - sub)
        t["receipt"].append(b["seen"] - sub)
        value = b["finalized"] - sub if "finalized" in b else INF
        no_final += value == INF
        final.append(value)
        t["final"].append(value)
    n = len(final)
    r["submit_to_finalized_s"] = spread(final)
    r["submit_to_receipt_s"] = spread(receipt)
    r["unfinalized_share"] = no_final / n if n else None
    r["unreceipted_share"] = no_receipt / n if n else None
    r["reverted_share"] = reverted_window / (n + reverted_window) if n + reverted_window else None
    r["per_type"] = type_metrics(per_type, finalized_hashes, {h: m for h, _, m in rows}, receipts, window)
    lag = [b["finalized"] - b["seen"] for b in in_window if "finalized" in b for _ in range(txs(b))]
    r["latest_to_finalized_s"] = spread(lag)
    ordered = sorted(in_window, key=lambda b: b["seen"])
    intervals = [(y["seen"] - x["seen"]) * 1000 for x, y in zip(ordered, ordered[1:])]
    r["block_interval_ms"] = spread(intervals, (50, 99)) | dict(mean=window * 1000 / len(in_window) if in_window else None)
    r["empty_block_share"] = sum(1 for b in in_window if not txs(b)) / len(in_window) if in_window else None
    r["largest_block_txs"] = max((txs(b) for b in in_window), default=None)
    with obs.lock:
        heads = [h for h in getattr(obs, "heads", []) if start <= h[0] <= end]
    spread_blocks = [max(v) - min(v) for _, v, _ in heads]
    r["validator_head_spread_blocks"] = spread(spread_blocks) | dict(max=max(spread_blocks, default=None))
    r["follower_present"] = bool(FOLLOWER_URLS)
    if FOLLOWER_URLS:
        lag = [max(v) - f for _, v, fs in heads for f in fs if f is not None]
        r["follower_lag_blocks"] = spread(lag) | dict(max=max(lag, default=None))
        # Seconds from node 1 first serving a height to the slowest follower serving it.
        first_seen = {n: b["seen"] for n, b in blocks.items()}
        reached, lag_s = {}, []
        for wall, _, fs in heads:
            if any(f is None for f in fs):
                continue
            for n in range(min(fs) - 50, min(fs) + 1):
                if n in first_seen and n not in reached:
                    reached[n] = wall
                    lag_s.append(wall - first_seen[n])
        r["follower_lag_s"] = spread(lag_s)
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
        span = last[0] - first[0]
        r["cpu_cores_nodes"] = {name: (ns - first[2][name]) / 1e9 / span for name, ns in last[2].items()
                                if name in first[2] and span > 0}
        r["cpu_s_per_tx_median"] = statistics.median(cpu.values()) if cpu else None
        rounds = delta(first[1][0], last[1][0], "reth_diesis_consensus_current_round")
        r["rounds_in_window"] = rounds
        r["cpu_s_per_round_nodes"] = {name: (ns - first[2][name]) / 1e9 / rounds for name, ns in last[2].items()
                                      if name in first[2] and rounds}
        r["throttled_s_per_s_nodes"], r["throttled_period_share_nodes"] = {}, {}
        if len(first) > 5 and len(last) > 5:
            for name, (ns, throttled, periods) in last[5].items():
                if name not in first[5] or span <= 0:
                    continue
                ns0, throttled0, periods0 = first[5][name]
                r["throttled_s_per_s_nodes"][name] = (ns - ns0) / 1e9 / span
                r["throttled_period_share_nodes"][name] = ((throttled - throttled0) / (periods - periods0)
                                                           if periods > periods0 else None)
        blocks1 = delta(first[1][0], last[1][0], "reth_sync_checkpoint", 'stage="Finish"')
        r["hold_ms_per_block_by_site_node1"] = dict(sorted(
            ((site.removeprefix("crates/node/src/dag_loop/"), ms / blocks1) for site, ms in
             by_label(first[1][0], last[1][0], "reth_diesis_lock_hold_ms_sum", "site",
                      'lock="selected_owner"').items() if blocks1),
            key=lambda kv: -kv[1])[:15])
        r["fallback_entries_by_cause_nodes"] = [by_label(first[1][i], last[1][i],
                                                         "reth_diesis_outcome_fallback_entries_total", "cause")
                                                for i in range(4)]
        r["fallback_resolved_by_path_nodes"] = [by_label(first[1][i], last[1][i],
                                                         "reth_diesis_outcome_fallback_resolved_total", "path")
                                                for i in range(4)]
        r["checkpoint_control_rejected_by_reason_nodes"] = [
            by_label(first[1][i], last[1][i], "reth_diesis_checkpoint_control_inbound_rejected_total", "reason")
            for i in range(4)]
        r["flow_held_ms_by_reason_node1"] = by_label(first[1][0], last[1][0],
                                                     "reth_diesis_horizon_flow_held_ms_total", "reason")
        r["tx_selection_nodes"] = [dict(primary=delta(first[1][i], last[1][i], "reth_diesis_tx_source_selected_primary_total"),
                                        fallback=delta(first[1][i], last[1][i], "reth_diesis_tx_source_selected_fallback_total"),
                                        owner_live_skips=delta(first[1][i], last[1][i],
                                                               "reth_diesis_tx_source_fallback_owner_live_total"))
                                   for i in range(4)]
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
    peaks = {}
    for sample in inner:
        for name, used in (sample[4] if len(sample) > 4 else {}).items():
            peaks[name] = max(peaks.get(name, 0), used)
    r["peak_memory_bytes_nodes"] = peaks
    loads = [s[3] for s in inner]
    r["host_load_1m"] = dict(median=statistics.median(loads), max=max(loads)) if loads else None
    r["generator_bound"] = r["attempted_tps"] < 0.95 * target_tps
    r["chain_saturated"] = (r["finalized_tps"] < 0.95 * r["attempted_tps"] or failed > 0.001 * max(attempted, 1)
                            or full_at is not None)
    r["validator_exits"] = obs.exited
    r["observer_errors"] = obs.errors
    return r


def type_metrics(per_type, finalized_hashes, methods, receipts, window):
    """Per method: successful finalized TPS and share, reverts, latency, gas used and spot-book fill
    shape (fills per successful transaction; takers that filled exactly two distinct makers)."""
    out = {}
    for h in finalized_hashes:
        method = methods.get(h, "unknown")
        t = per_type.setdefault(method, dict(submitted_in_window=0, reverted_in_window=0, receipt=[], final=[]))
        rec = receipts.get(h)
        key = "status_unknown" if rec is None else ("successful" if rec["status"] == 1 else "reverted")
        t[key] = t.get(key, 0) + 1
        if rec is None:
            continue
        t.setdefault("gas_" + key, []).append(rec["gas"])
        if rec["status"] == 1:
            fills = t.setdefault("fills", {})
            fills[str(len(rec["makers"]))] = fills.get(str(len(rec["makers"])), 0) + 1
            t["two_distinct"] = t.get("two_distinct", 0) + (len(rec["makers"]) == 2 == len(set(rec["makers"])))
    total = sum(t.get("successful", 0) for t in per_type.values())
    mean = lambda xs: round(statistics.fmean(xs)) if xs else None
    for method, t in sorted(per_type.items()):
        ok = t.get("successful", 0)
        out[method] = dict(
            submitted_in_window=t["submitted_in_window"], reverted_in_window=t["reverted_in_window"],
            finalized_successful=ok if receipts else None,
            finalized_successful_tps=ok / window if receipts else None,
            share_of_successful=ok / total if total else None,
            finalized_reverted=t.get("reverted", 0) if receipts else None,
            finalized_status_unknown=t.get("status_unknown", 0),
            submit_to_finalized_s=spread(t["final"]), submit_to_receipt_s=spread(t["receipt"]),
            gas_used_avg=mean(t.get("gas_successful")), gas_used_avg_reverted=mean(t.get("gas_reverted")),
            fills=t.get("fills", {}), two_distinct_makers=t.get("two_distinct", 0))
    return out


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


# Reth's file log keeps every timeline line at its default (debug) and at info. When the file filter
# is debug and stdout is info, stdout drops some: an info event right after a debug-level `enabled!`
# check on the same thread can miss the stdout layer. Matching filters (judged runs: info) drop none.
NODE_FILE_LOGS = "/root/.cache/reth/logs/*/reth.log*"


def file_timeline(container, path, since_unix):
    """Copy a validator's timeline lines from its reth file logs (all rotations) to `path`, keeping
    lines at or after `since_unix`. Returns the line count, or None when the logs are unreadable."""
    pattern = "round timeline|mpt vote timeline"
    try:
        out = subprocess.run(["docker", "exec", container, "sh", "-c", f"grep -h -E '{pattern}' {NODE_FILE_LOGS}"],
                             capture_output=True, text=True, errors="replace", timeout=300)
    except Exception:
        return None
    if out.returncode not in (0, 1):
        return None
    since_us = int(since_unix * 1e6)
    kept = 0
    with open(path, "w") as f:
        for line in out.stdout.splitlines(keepends=True):
            m = re.search(r"unix_us=(\d+)", line)
            if m and int(m.group(1)) >= since_us:
                f.write(LOG_ANSI.sub("", line))
                kept += 1
    return kept


SAMPLED_TX = re.compile(r'event="tx_admitted" tx=(0x[0-9a-fA-F]{64})')


def sampled_tx_blocks(obs, run_dir):
    """Block number, node-1 first-seen and all-four finalized times of every transaction the
    validators sampled on the round timeline (DIESIS_TRACE_TX_SAMPLE), for diesis-timeline.py."""
    hashes = set()
    for path in run_dir.glob("timeline-*.log"):
        with open(path, errors="replace") as f:
            hashes.update(m.group(1).lower() for line in f if (m := SAMPLED_TX.search(line)))
    if not hashes:
        return 0
    out = {}
    with obs.lock:
        for number, block in obs.blocks.items():
            for h in block.get("hashes") or ():
                if h.lower() in hashes:
                    out[h.lower()] = dict(number=number, seen=block.get("seen"), finalized=block.get("finalized"))
        blocks = {n: dict(seen=b.get("seen"), finalized=b.get("finalized"), txs=len(b.get("hashes") or ()))
                  for n, b in obs.blocks.items()}
    (run_dir / "timeline-blocks.json").write_text(json.dumps(dict(sampled=out, blocks=blocks)) + "\n")
    return len(out)


LOG_ANSI = re.compile(r"\x1b\[[0-9;]*m")
WORK_REFUSED = re.compile(r'Work refused; (retrying once Work frees|not retrying) operation="?(\w+)')


def count_log_line(line, counts):
    """Tally one validator log line: unknown parent vertex errors, selected-owner Work refusals by
    operation (retried or not), and Work-admission limit errors."""
    line = LOG_ANSI.sub("", line)
    if "unknown parent vertex" in line:
        counts["unknown_parent_vertex"] += 1
    m = WORK_REFUSED.search(line)
    if m:
        kind = "retried" if m[1].startswith("retrying") else "not_retried"
        by = counts["work_refused"].setdefault(m[2], dict(retried=0, not_retried=0))
        by[kind] += 1
    if "Work admission:" in line and "configured limit" in line:
        counts["work_admission_limit_errors"] += 1


class LogCounts:
    """Follow every validator container's log for the run (docker logs -f from the observer's
    start) and count lines with count_log_line. Streaming survives the fleet's log rotation and the
    containers' removal at teardown."""

    def __init__(self, containers, run_dir=None):
        self.counts = {c: dict(unknown_parent_vertex=0, work_refused={}, work_admission_limit_errors=0)
                       for c in containers}
        self.procs, self.sinks = [], []
        since = f"{time.time() - 1:.3f}"
        for name in containers:
            proc = subprocess.Popen(["docker", "logs", "-f", "--since", since, name], stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, text=True, errors="replace")
            self.procs.append(proc)
            sink = open(run_dir / f"timeline-{name}.log", "w") if run_dir is not None else None
            if sink is not None:
                self.sinks.append(sink)
            threading.Thread(target=self.read, args=(proc, self.counts[name], sink), daemon=True).start()

    @staticmethod
    def read(proc, counts, sink=None):
        for line in proc.stdout:
            count_log_line(line, counts)
            if sink is not None and any(target in line for target in TIMELINE_TARGETS):
                sink.write(LOG_ANSI.sub("", line))

    def stop(self):
        for proc in self.procs:
            proc.terminate()
        time.sleep(0.5)
        for sink in self.sinks:
            sink.close()
        return self.counts


def watch(a):
    obs = Observer()
    logs = LogCounts(obs.containers, a.run_dir)
    obs.run_dir = a.run_dir
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
    rows = read_submit_log(a.run_dir / "submit-log.csv")
    load_start = min(t for _, t, _ in rows) if rows else time.time()
    with obs.lock:
        numbers = [n for n, b in obs.blocks.items() if b.get("hashes") and b.get("seen", 0) >= load_start - 5]
    receipt_errors = []
    receipts = fetch_receipts(numbers, receipt_errors) if a.receipts else None
    metrics = window_metrics(obs, a.run_dir / "submit-log.csv", report, a.target_tps, a.duration, a.warmup,
                             a.pool_cap, receipts=receipts)
    metrics["receipt_errors"] = receipt_errors[:20]
    metrics["drain_s"] = time.time() - load_end
    metrics["finalized_agreement"] = agreement(obs, metrics["window_start"], metrics["window_end"])
    metrics["pool_pending_after_drain"] = pool
    metrics["log_counts"] = logs.stop()
    # Replace the stdout copies with the complete file-log timeline where readable.
    metrics["timeline_source"] = {}
    for name in obs.containers:
        kept = file_timeline(name, a.run_dir / f"timeline-{name}.log", obs.started)
        metrics["timeline_source"][name] = "stdout" if kept is None else f"file ({kept} lines)"
    metrics["timeline_sampled_txs_in_blocks"] = sampled_tx_blocks(obs, a.run_dir)
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
    print(f"  Finalized TPS:          {m['finalized_tps']:.1f}  ({m.get('finalized_basis', 'included')}; "
          f"attempted {m['attempted_tps']:.1f}, target {m['target_tps']})")
    o = m.get("outcomes") or {}
    if o.get("finalized_reverted") is not None:
        print(f"  Finalized outcomes:     successful {o['finalized_successful']}  reverted {o['finalized_reverted']}  "
              f"status unknown {o['finalized_status_unknown']}  (included TPS {m['finalized_included_tps']:.1f})")
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
    cores = m.get("cpu_cores_nodes") or {}
    throttled = m.get("throttled_s_per_s_nodes") or {}
    per_round = m.get("cpu_s_per_round_nodes") or {}
    print(f"  CPU cores (per node):   {', '.join(f'{v:.2f}' for v in cores.values()) or '-'}  "
          f"ms/round {', '.join(f'{v * 1000:.0f}' for v in per_round.values()) or '-'}  "
          f"throttled s/s {', '.join(f'{v:.3f}' for v in throttled.values()) or '-'}")
    o = m.get("outcomes") or {}
    print(f"  Receipts:               succeeded {o.get('finalized_successful')}  reverted {o.get('finalized_reverted')}  "
          f"largest block {m.get('largest_block_txs')} tx  head spread p99 "
          f"{(m.get('validator_head_spread_blocks') or {}).get('p99')}  follower "
          f"{(m.get('follower_lag_blocks') or {}).get('p99') if m.get('follower_present') else 'none'}")
    types_ = m.get("per_type") or {}
    if len(types_) > 1 or "unknown" not in types_:
        print("  Per type:               share  ok TPS  reverted  fin p50/p99 s  rcpt p50 s  gas used  fills")
        for name, t in types_.items():
            print(f"    {name:<20} {fmt(t['share_of_successful'])}  {fmt(t['finalized_successful_tps'])}  "
                  f"{fmt(t['finalized_reverted'])}  {fmt(t['submit_to_finalized_s']['p50'])}/"
                  f"{fmt(t['submit_to_finalized_s']['p99'])}  {fmt(t['submit_to_receipt_s']['p50'])}  "
                  f"{fmt(t['gas_used_avg'])}  {t['fills']}")
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
    w.add_argument("--no-receipts", dest="receipts", action="store_false",
                   help="Skip the post-drain receipt pass (finalized TPS then counts included transactions)")
    s = sub.add_parser("summary")
    s.add_argument("--metrics", type=Path, required=True)
    a = p.parse_args()
    if a.cmd == "watch":
        watch(a)
    else:
        print_summary(json.loads(a.metrics.read_text()))


if __name__ == "__main__":
    main()
