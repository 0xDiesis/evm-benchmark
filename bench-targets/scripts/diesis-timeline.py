#!/usr/bin/env python3
"""Stage attribution and inclusion-wait analysis from the Diesis round timeline.

The node logs round-path marks with DIESIS_TRACE_ROUND_TIMELINE=true, per-transaction marks for one
hash in N with DIESIS_TRACE_TX_SAMPLE=N, and vote-path marks with DIESIS_TRACE_MPT_VOTE_TIMELINE=true.
diesis-observer.py copies those lines to <run-dir>/timeline-<container>.log and writes
timeline-blocks.json (block of every sampled transaction). Validators share the host clock.

  diesis-timeline.py analyze --run-dir DIR       stages.json + a printed summary
  diesis-timeline.py probe --out DIR [--count 80] [--senders 8] [--settle 60]
      idle inclusion probes on a fresh fleet (fleet.py env: DIESIS_REPO_DIR, DIESIS_IMAGE_TAG,
      BENCH_DIESIS_PROFILE, BENCH_DIESIS_ENV, BENCH_DIESIS_FLEET, BENCH_DIESIS_MAX_FLEETS): one
      transfer at a time, round robin over the four RPC endpoints and --senders funded senders, each
      after the previous receipt plus a random 0.2-1.2 s; then the validators' timeline lines, the
      probe blocks and `analyze`. The fleet is removed at the end unless --keep.

Per sampled transaction:
  submit node and time   the validator whose pool admitted it with origin `local` (RPC)
  first opportunity R0   the submit node's first local vertex signed after that admission
  owner at R0            the sender partition owner at R0, from any proposer's selection verdict
  carry                  the first signed local vertex whose sampled hashes include it
  extra rounds           carry round - R0 (0: carried at the first opportunity)
Stages (plan section 1.2) for a sampled transaction in block N cut from the Commit of leader round L:
  a submit -> carrying vertex signed; b carrying vertex -> leader vertex of L signed;
  c leader vertex -> round L+3 entered; d1 round L+3 -> decided; d2 decided -> installed;
  f installed -> block N cut; g cut -> vote requested; h requested -> quorum;
  i quorum -> node 1 serves N; fin node 1 serves N -> all-four finalized.
Node-local stages use node 1's marks when it logged them, else the earliest validator's.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import random
import re
import statistics
import subprocess
import sys
import time
from urllib.request import Request, urlopen

HERE = Path(__file__).resolve().parent
FIELD = re.compile(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)')
NODE = re.compile(r"node-(\d+)")
DEV_KEY = "0x" + "0" * 63 + "1"
SINK = "0x000000000000000000000000000000000000dEaD"


def parse_line(line):
    if "round timeline" not in line and "mpt vote timeline" not in line:
        return None
    fields = {k: v.strip('"') for k, v in FIELD.findall(line.split(": ", 1)[-1])}
    if "event" not in fields or "unix_us" not in fields:
        return None
    fields["kind"] = "vote" if "mpt vote timeline" in line else "round"
    fields["t"] = int(fields["unix_us"]) / 1e6
    return fields


def load_events(run_dir):
    events = []
    for path in sorted(Path(run_dir).glob("timeline-*.log")):
        m = NODE.search(path.name)
        node = int(m.group(1)) if m else 0
        with open(path, errors="replace") as f:
            for line in f:
                e = parse_line(line)
                if e:
                    e["node"] = node
                    events.append(e)
    events.sort(key=lambda e: e["t"])
    return events


def pct(values, p):
    values = sorted(v for v in values if v is not None)
    if not values:
        return None
    return values[max(0, -(-len(values) * p // 100) - 1)]


def summary(values):
    values = [v for v in values if v is not None]
    return dict(n=len(values), p50=pct(values, 50), p90=pct(values, 90),
                mean=statistics.fmean(values) if values else None)


def first(items, key=lambda e: e["t"]):
    items = list(items)
    return min(items, key=key) if items else None


def analyze(run_dir):
    run_dir = Path(run_dir)
    events = load_events(run_dir)
    blocks_path = run_dir / "timeline-blocks.json"
    blocks = json.loads(blocks_path.read_text()) if blocks_path.exists() else dict(sampled={}, blocks={})
    by = {}
    for e in events:
        by.setdefault((e["kind"], e["event"]), []).append(e)

    signed = by.get(("round", "vertex_signed"), [])
    rounds_by_node = {}
    for e in signed:
        rounds_by_node.setdefault(e["node"], []).append((e["t"], int(e["round"])))
    carriers, leader_vertex, round_entry = {}, {}, {}
    for e in signed:
        r = int(e["round"])
        round_entry[r] = min(round_entry.get(r, e["t"]), e["t"])
        if e.get("leader") == "true":
            leader_vertex[r] = min(leader_vertex.get(r, e["t"]), e["t"])
        for h in filter(None, e.get("sampled", "").split(",")):
            h = h.lower()
            if h not in carriers or e["t"] < carriers[h]["t"]:
                carriers[h] = dict(t=e["t"], node=e["node"], round=r)
    # Graph round entry from the vote timeline's "round" marks when present.
    for e in by.get(("vote", "round"), []):
        r = int(e["number"])
        round_entry[r] = min(round_entry.get(r, e["t"]), e["t"])

    admitted, selections, slot_of = {}, {}, {}
    for e in by.get(("round", "tx_admitted"), []):
        admitted.setdefault(e["tx"].lower(), []).append(e)
    for e in by.get(("round", "tx_selection"), []):
        selections.setdefault(e["tx"].lower(), []).append(e)
        slot_of.setdefault(e["node"], int(e["local"]))

    def node_pref(items):
        items = list(items)
        node1 = [e for e in items if e["node"] == 1]
        return first(node1) or first(items)

    def keyed(kind, event, field):
        out = {}
        for e in by.get((kind, event), []):
            out.setdefault(int(e[field]), []).append(e)
        return out

    decided, installed = keyed("round", "decided", "round"), keyed("round", "installed", "round")
    cut = keyed("round", "block_cut", "number")
    requested, quorum = keyed("vote", "requested", "number"), keyed("vote", "quorum", "number")

    txs, inclusion = [], []
    for h, adm in admitted.items():
        local = [e for e in adm if e["origin"] == "local"]
        if not local:
            continue
        sub = first(local)
        submit_node, t0 = sub["node"], sub["t"]
        gossip = [e["t"] - t0 for e in adm if e["origin"] == "external"]
        r0 = next((r for t, r in sorted(rounds_by_node.get(submit_node, [])) if t >= t0), None)
        owner = next((int(e["owner"]) for e in selections.get(h, []) if r0 is not None and int(e["round"]) == r0), None)
        carry = carriers.get(h)
        row = dict(tx=h, submit_node=submit_node, submit=t0, r0=r0,
                   owner_is_submitter=None if owner is None or submit_node not in slot_of else owner == slot_of[submit_node],
                   gossip_s=max(gossip) if len(gossip) >= 3 else None,
                   carry_node=carry and carry["node"], carry_round=carry and carry["round"],
                   extra_rounds=None if not carry or r0 is None else carry["round"] - r0,
                   a=None if not carry else carry["t"] - t0)
        info = blocks["sampled"].get(h)
        if info and carry:
            number = info["number"]
            cut_e = node_pref(cut.get(number, []))
            if cut_e:
                lr = int(cut_e["round"])
                t_lead = leader_vertex.get(lr)
                t_l3 = round_entry.get(lr + 3)
                dec, ins = node_pref(decided.get(lr, [])), node_pref(installed.get(lr, []))
                req, quo = first(requested.get(number, [])), first(quorum.get(number, []))

                def d(x, y):
                    return None if x is None or y is None else y - x
                row.update(block=number, leader_round=lr, rounds_carry_to_leader=lr - carry["round"],
                           b=d(carry["t"], t_lead), c=d(t_lead, t_l3), d1=d(t_l3, dec and dec["t"]),
                           d2=d(dec and dec["t"], ins and ins["t"]), f=d(ins and ins["t"], cut_e["t"]),
                           g=d(cut_e["t"], req and req["t"]), h=d(req and req["t"], quo and quo["t"]),
                           i=d(quo and quo["t"], info.get("seen")), fin=d(info.get("seen"), info.get("finalized")),
                           total=d(t0, info.get("finalized")))
        txs.append(row)
        inclusion.append(row)

    # Round period from node 1's signed rounds (consecutive rounds only).
    periods = []
    for node, seq in rounds_by_node.items():
        seq.sort()
        periods += [(t2 - t1) / (r2 - r1) for (t1, r1), (t2, r2) in zip(seq, seq[1:]) if 0 < r2 - r1 <= 2]
    round_period = summary(periods)

    def split(rows):
        carried = [r for r in rows if r["extra_rounds"] is not None]
        return dict(n=len(rows), carried=len(carried),
                    first_opportunity_share=(sum(1 for r in carried if r["extra_rounds"] <= 0) / len(carried)
                                             if carried else None),
                    extra_rounds=summary([r["extra_rounds"] for r in carried]),
                    a_s=summary([r["a"] for r in carried]),
                    carried_by_submitter_share=(sum(1 for r in carried if r["carry_node"] == r["submit_node"])
                                                / len(carried) if carried else None))

    stages = {k: summary([r.get(k) for r in txs]) for k in ("a", "b", "c", "d1", "d2", "f", "g", "h", "i", "fin",
                                                            "total")}
    modes = {}
    for e in signed:
        modes[e.get("mode", "?")] = modes.get(e.get("mode", "?"), 0) + 1
    fallback = {}
    for e in by.get(("round", "fallback"), []):
        if e["node"] == 1:
            fallback[e["cause"]] = fallback.get(e["cause"], 0) + 1
    resolved = {}
    for e in by.get(("round", "fallback_resolved"), []):
        if e["node"] == 1:
            resolved[e["path"]] = resolved.get(e["path"], 0) + 1
    out = dict(sampled_txs=len(txs), round_period_s=round_period, stages_s=stages,
               rounds_carry_to_leader=summary([r.get("rounds_carry_to_leader") for r in txs]),
               gossip_to_all_s=summary([r["gossip_s"] for r in txs]),
               inclusion=dict(all=split(inclusion),
                              owner=split([r for r in inclusion if r["owner_is_submitter"] is True]),
                              non_owner=split([r for r in inclusion if r["owner_is_submitter"] is False]),
                              unknown_owner=split([r for r in inclusion if r["owner_is_submitter"] is None])),
               payload_modes=modes, fallback_node1=dict(entries=fallback, resolved=resolved),
               slots=slot_of)
    (run_dir / "stages.json").write_text(json.dumps(dict(summary=out, txs=txs), indent=1) + "\n")
    return out


def fmt(v, scale=1000, unit="ms"):
    return "-" if v is None else f"{v * scale:.0f}{unit}"


def print_summary(s):
    T = s["round_period_s"]["p50"]
    print(f"sampled transactions {s['sampled_txs']}; round period p50 {fmt(T)}")
    for k, v in s["stages_s"].items():
        print(f"  stage {k:5s} p50 {fmt(v['p50']):>7s}  p90 {fmt(v['p90']):>7s}  n {v['n']}"
              + (f"  ({v['p50'] / T:.2f} T)" if T and v["p50"] is not None and k != "total" else ""))
    for name, row in s["inclusion"].items():
        share = row["first_opportunity_share"]
        mean = row["extra_rounds"]["mean"]
        print(f"  {name:13s} n {row['n']:4d} carried {row['carried']:4d} first-opportunity "
              f"{'-' if share is None else f'{share:.2f}'} extra rounds p50 {row['extra_rounds']['p50']} "
              f"mean {'-' if mean is None else f'{mean:.2f}'} a p50 {fmt(row['a_s']['p50'])}")
    print(f"  payload modes {s['payload_modes']}; fallback (node 1) {s['fallback_node1']}")


# ---- idle probe arm -------------------------------------------------------------------------

def load_module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def rpc(url, method, params=(), timeout=10):
    body = json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=list(params))).encode()
    with urlopen(Request(url, body, {"Content-Type": "application/json"}), timeout=timeout) as response:
        data = json.load(response)
    if "error" in data:
        raise RuntimeError(f"{method}: {data['error']}")
    return data["result"]


def cast(*args):
    return subprocess.check_output(["cast", *args], text=True).strip()


def transfer(key, chain_id, nonce, gas_price, to, value):
    return cast("mktx", "--private-key", key, "--chain", str(chain_id), "--nonce", str(nonce), "--gas-price",
                str(gas_price), "--gas-limit", "21000", "--legacy", to, "--value", str(value))


def wait_receipt(urls, tx, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for url in urls:
            try:
                receipt = rpc(url, "eth_getTransactionReceipt", [tx])
                if receipt:
                    return receipt
            except Exception:
                pass
        time.sleep(0.1)
    return None


def probe(a):
    fleet = load_module(HERE.parent / "chains/diesis/fleet.py", "fleet")
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    with open(out / "fleet.log", "w") as log:
        subprocess.run([sys.executable, "-B", str(HERE.parent / "chains/diesis/fleet.py"), "reset"], env=env,
                       check=True, stdout=log, stderr=subprocess.STDOUT)
    project = fleet.fresh_project()
    ports = fleet.project_ports(project)
    urls = [f"http://127.0.0.1:{p}" for p in ports["rpc"]]
    containers = ports.get("containers") or [f"{project}-node-{i}" for i in range(1, 5)]
    started = time.time()
    record = dict(project=project, urls=urls, containers=containers, env=env.get("BENCH_DIESIS_ENV"),
                  profile=env.get("BENCH_DIESIS_PROFILE"), host_load_start=os.getloadavg(), probes=[])
    try:
        time.sleep(a.settle)
        chain_id = int(rpc(urls[0], "eth_chainId"), 16)
        gas_price = int(rpc(urls[0], "eth_gasPrice"), 16) * 2
        keys = ["0x" + f"{0xd1e5 + i:064x}" for i in range(a.senders)]
        dev = cast("wallet", "address", "--private-key", DEV_KEY)
        nonce = int(rpc(urls[0], "eth_getTransactionCount", [dev, "pending"]), 16)
        funding = []
        for key in keys:
            addr = cast("wallet", "address", "--private-key", key)
            funding.append(rpc(urls[0], "eth_sendRawTransaction",
                               [transfer(DEV_KEY, chain_id, nonce, gas_price, addr, 10**18)]))
            nonce += 1
        for tx in funding:
            if not wait_receipt(urls, tx):
                raise SystemExit(f"funding {tx} not included")
        nonces = [0] * a.senders
        rng = random.Random(a.seed)
        for i in range(a.count):
            node, s = i % 4, i % a.senders
            raw = transfer(keys[s], chain_id, nonces[s], gas_price, SINK, 1)
            wall = time.time()
            tx = rpc(urls[node], "eth_sendRawTransaction", [raw])
            nonces[s] += 1
            receipt = wait_receipt(urls, tx)
            record["probes"].append(dict(tx=tx.lower(), node=node + 1, sender=s, submitted=wall,
                                         block=receipt and int(receipt["blockNumber"], 16),
                                         receipt_s=None if not receipt else time.time() - wall,
                                         host_load_1m=os.getloadavg()[0]))
            time.sleep(rng.uniform(0.2, 1.2))
        # Let the last probes finalize before reading blocks.
        time.sleep(5)
        sampled, blocks = {}, {}
        for p in record["probes"]:
            if p["block"] is None:
                continue
            block = rpc(urls[0], "eth_getBlockByNumber", [hex(p["block"]), False])
            sampled[p["tx"]] = dict(number=p["block"], seen=p["submitted"] + p["receipt_s"], finalized=None)
            blocks[p["block"]] = dict(txs=len(block["transactions"]))
        (out / "timeline-blocks.json").write_text(json.dumps(dict(sampled=sampled, blocks=blocks)) + "\n")
        observer = load_module(HERE / "diesis-observer.py", "diesis_observer")
        record["timeline_lines"] = {name: observer.file_timeline(name, out / f"timeline-{name}.log", started)
                                    for name in containers}
        record["host_load_end"] = os.getloadavg()
    finally:
        (out / "probes.json").write_text(json.dumps(record, indent=1) + "\n")
        if not a.keep:
            with open(out / "fleet.log", "a") as log:
                subprocess.run([sys.executable, "-B", str(HERE.parent / "chains/diesis/fleet.py"), "down"], env=env,
                               stdout=log, stderr=subprocess.STDOUT)
    s = analyze(out)
    print_summary(s)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    an = sub.add_parser("analyze")
    an.add_argument("--run-dir", required=True)
    pr = sub.add_parser("probe")
    pr.add_argument("--out", required=True)
    pr.add_argument("--count", type=int, default=80)
    pr.add_argument("--senders", type=int, default=8)
    pr.add_argument("--settle", type=int, default=60)
    pr.add_argument("--seed", type=int, default=1)
    pr.add_argument("--keep", action="store_true")
    a = p.parse_args()
    if a.cmd == "analyze":
        print_summary(analyze(a.run_dir))
    else:
        probe(a)


if __name__ == "__main__":
    main()
