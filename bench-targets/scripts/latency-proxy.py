#!/usr/bin/env python3
"""Observe transaction response, receipt and finalized visibility independently."""

import argparse
from contextlib import nullcontext
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
from pathlib import Path
import signal
import threading
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen


def percentiles(values):
    ordered = sorted(values)
    if not ordered:
        return None
    return {
        **{
            f"p{p}": ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)]
            for p in (50, 95, 99)
        },
        "max": ordered[-1],
        "count": len(ordered),
    }


class Observer:
    def __init__(self, upstreams, output, chain_id):
        self.upstreams, self.output, self.chain_id = upstreams, output, chain_id
        self.limit = threading.BoundedSemaphore(4)
        # Keep observation and client reads from racing the smallest validator's
        # read admission bound. Apply the same bound to every chain.
        self.read_limits = {url: threading.BoundedSemaphore(1) for url in upstreams}
        self.lock = threading.RLock()
        self.stop = threading.Event()
        self.transactions, self.receipts, self.finalized = {}, {}, {}
        self.blocks = {}
        self.errors, self.failures, self.duplicates = 0, 0, 0
        self.last = 0
        self.events = None

    def emit(self, event, **fields):
        with self.lock:
            self.events.write(
                json.dumps(
                    dict(event=event, monotonic_ns=time.monotonic_ns(), **fields)
                )
                + "\n"
            )
            self.events.flush()

    def request(self, upstream, body):
        requests = json.loads(body)
        requests = requests if isinstance(requests, list) else [requests]
        methods = [request.get("method", "") for request in requests]
        bounded_read = any(
            method.startswith(("eth_get", "eth_call", "eth_estimateGas"))
            for method in methods
        )
        queued = time.monotonic_ns()
        with (
            self.read_limits[upstream] if bounded_read else nullcontext(),
            self.limit,
        ):
            started = time.monotonic_ns()
            request = Request(upstream, body, {"Content-Type": "application/json"})
            try:
                response = urlopen(request, timeout=5)
            except HTTPError as error:
                response = error
            with response:
                encoded, status = response.read(), response.code
            completed = time.monotonic_ns()
        if self.events:
            self.emit(
                "rpc_transport",
                upstream=upstream,
                methods=methods,
                queued_ns=queued,
                started_ns=started,
                completed_ns=completed,
                queue_ms=(started - queued) / 1e6,
                upstream_ms=(completed - started) / 1e6,
            )
        return status, encoded, completed

    def rpc(self, upstream, method, params):
        started = time.monotonic_ns()
        try:
            status, encoded, completed = self.request(
                upstream,
                json.dumps(
                    dict(jsonrpc="2.0", id=1, method=method, params=params)
                ).encode(),
            )
        except Exception as error:
            if self.events:
                self.emit(
                    "rpc_error",
                    method=method,
                    upstream=upstream,
                    started_ns=started,
                    error=str(error),
                )
            raise RuntimeError(f"{method} at {upstream}: {error}") from error
        if self.events:
            self.emit(
                "rpc_observation",
                method=method,
                upstream=upstream,
                started_ns=started,
                completed_ns=completed,
            )
        response = json.loads(encoded)
        if status != 200 or "error" in response:
            raise ValueError(f"{method}: HTTP {status}: {response.get('error')}")
        return response["result"], completed

    def initialize(self):
        for upstream in self.upstreams:
            chain_id, _ = self.rpc(upstream, "eth_chainId", [])
            if int(chain_id, 16) != self.chain_id:
                raise ValueError("Upstream chain ID mismatch")
        height, _ = self.rpc(self.upstreams[0], "eth_blockNumber", [])
        self.last = int(height, 16)
        self.output.mkdir(parents=True, exist_ok=False)
        self.events = (self.output / "events.jsonl").open("x")

    def submitted(self, requests, responses, upstream, started, completed):
        requests = requests if isinstance(requests, list) else [requests]
        responses = responses if isinstance(responses, list) else [responses]
        by_id = {
            response.get("id"): response
            for response in responses
            if isinstance(response, dict)
        }
        with self.lock:
            for request in requests:
                if request.get("method") != "eth_sendRawTransaction":
                    continue
                response = by_id.get(request.get("id"), {})
                tx_hash = response.get("result")
                if (
                    not isinstance(tx_hash, str)
                    or not tx_hash.startswith("0x")
                    or len(tx_hash) != 66
                ):
                    self.failures += 1
                    self.emit(
                        "submission_failed",
                        upstream=upstream,
                        started_ns=started,
                        completed_ns=completed,
                        error=response.get("error", "No transaction hash"),
                    )
                    continue
                if tx_hash in self.transactions:
                    self.duplicates += 1
                    self.emit("duplicate_submission", tx_hash=tx_hash)
                    continue
                self.transactions[tx_hash] = dict(
                    started_ns=started, responded_ns=completed
                )
                self.emit(
                    "submission",
                    tx_hash=tx_hash,
                    upstream=upstream,
                    started_ns=started,
                    completed_ns=completed,
                    latency_ms=(completed - started) / 1e6,
                )
            self.update()

    def update(self):
        for tx_hash, transaction in self.transactions.items():
            if "receipt" not in transaction and tx_hash in self.receipts:
                receipt = self.receipts[tx_hash]
                transaction["receipt"] = receipt
                self.emit(
                    "receipt",
                    tx_hash=tx_hash,
                    **receipt,
                    latency_ms=max(
                        0, receipt["completed_ns"] - transaction["started_ns"]
                    )
                    / 1e6,
                )
            receipt = transaction.get("receipt")
            finalized = self.finalized.get(receipt["block_number"]) if receipt else None
            if finalized and "finalized_ns" not in transaction:
                transaction["finalized_ns"] = finalized
                self.emit(
                    "finalized",
                    tx_hash=tx_hash,
                    completed_ns=finalized,
                    block_number=receipt["block_number"],
                    block_hash=receipt["block_hash"],
                    latency_ms=max(0, finalized - transaction["started_ns"]) / 1e6,
                )

    def scan(self):
        height, _ = self.rpc(self.upstreams[0], "eth_blockNumber", [])
        for number in range(self.last + 1, min(int(height, 16), self.last + 16) + 1):
            block, _ = self.rpc(
                self.upstreams[0], "eth_getBlockByNumber", [hex(number), False]
            )
            receipts, completed = self.rpc(
                self.upstreams[0], "eth_getBlockReceipts", [hex(number)]
            )
            if not block or not isinstance(receipts, list):
                break
            # An empty/not-yet-indexed receipt response must not consume the height.
            if {r.get("transactionHash") for r in receipts} != set(
                block["transactions"]
            ):
                break
            with self.lock:
                for receipt in receipts:
                    if (
                        receipt["blockHash"] != block["hash"]
                        or int(receipt["blockNumber"], 16) != number
                    ):
                        raise ValueError("Receipt canonical block mismatch")
                    self.receipts[receipt["transactionHash"]] = dict(
                        block_number=number,
                        block_hash=block["hash"],
                        status=int(receipt["status"], 16),
                        completed_ns=completed,
                    )
                if receipts:
                    self.blocks[number] = block["hash"]
                self.last = number
                self.update()

    def sample_finality(self):
        heads = [
            self.rpc(upstream, "eth_getBlockByNumber", ["finalized", False])[0]
            for upstream in self.upstreams
        ]
        if not all(heads):
            return
        floor = min(int(head["number"], 16) for head in heads)
        with self.lock:
            pending = [
                (number, block_hash)
                for number, block_hash in self.blocks.items()
                if number <= floor and number not in self.finalized
            ]
        for number, block_hash in pending:
            blocks = [
                self.rpc(upstream, "eth_getBlockByNumber", [hex(number), False])
                for upstream in self.upstreams
            ]
            if not all(block and block["hash"] == block_hash for block, _ in blocks):
                raise ValueError("Finalized canonical hash disagreement")
            with self.lock:
                self.finalized[number] = max(completed for _, completed in blocks)
                self.update()

    def summary(self, finished=False):
        with self.lock:
            transactions = list(self.transactions.values())
            received = [t for t in transactions if "receipt" in t]
            finalized = [t for t in transactions if "finalized_ns" in t]
            reverted = sum(t["receipt"]["status"] != 1 for t in received)
            passed = (
                bool(transactions)
                and not (self.failures or self.errors or self.duplicates or reverted)
                and len(finalized) == len(transactions)
            )
            return dict(
                status=("passed" if passed else "failed") if finished else "running",
                chain_id=self.chain_id,
                accepted=len(transactions),
                failed_submissions=self.failures,
                duplicate_submissions=self.duplicates,
                reverted=reverted,
                missing_receipts=len(transactions) - len(received),
                missing_finality=len(transactions) - len(finalized),
                observer_errors=self.errors,
                submission_latency_ms=percentiles(
                    [(t["responded_ns"] - t["started_ns"]) / 1e6 for t in transactions]
                ),
                receipt_latency_ms=percentiles(
                    [
                        max(0, t["receipt"]["completed_ns"] - t["started_ns"]) / 1e6
                        for t in received
                    ]
                ),
                finality_latency_ms=percentiles(
                    [
                        max(0, t["finalized_ns"] - t["started_ns"]) / 1e6
                        for t in finalized
                    ]
                ),
            )

    def atomic_json(self, name, content):
        path = self.output / name
        temporary = path.with_suffix(path.suffix + ".tmp")
        temporary.write_text(json.dumps(content, indent=2) + "\n")
        temporary.replace(path)

    def monitor(self, interval):
        next_finality = 0
        while not self.stop.is_set():
            try:
                self.scan()
                if time.monotonic() >= next_finality:
                    self.sample_finality()
                    next_finality = time.monotonic() + 0.2
            except Exception as error:
                with self.lock:
                    self.errors += 1
                    self.emit("observer_error", error=str(error))
            self.atomic_json("summary.json", self.summary())
            self.stop.wait(interval)


def handler_for(observer, upstream):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            started = time.monotonic_ns()
            try:
                body = self.rfile.read(int(self.headers["Content-Length"]))
                requests = json.loads(body)
                status, encoded, completed = observer.request(upstream, body)
                observer.submitted(
                    requests, json.loads(encoded), upstream, started, completed
                )
            except Exception as error:
                status, encoded = 502, json.dumps(dict(error=str(error))).encode()
                with observer.lock:
                    observer.errors += 1
                    observer.emit("proxy_error", error=str(error))
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--upstreams", required=True, help="Comma-separated validator HTTP URLs"
    )
    parser.add_argument(
        "--listen-base",
        type=int,
        default=39545,
        help="Four consecutive loopback ports; zero selects ephemeral ports",
    )
    parser.add_argument(
        "--out",
        type=Path,
        required=True,
        help="New directory for raw events, ready.json and atomic summary.json",
    )
    parser.add_argument("--chain-id", type=int, required=True)
    parser.add_argument("--poll-interval", type=float, default=0.05)
    args = parser.parse_args()
    upstreams = args.upstreams.split(",")
    if len(upstreams) != 4 or args.poll_interval < 0.01:
        parser.error(
            "Exactly four upstream validators and poll interval >= 0.01 are required"
        )
    observer = Observer(upstreams, args.out, args.chain_id)
    observer.initialize()
    servers, threads = [], []
    signal.signal(signal.SIGTERM, lambda *_: observer.stop.set())
    signal.signal(signal.SIGINT, lambda *_: observer.stop.set())
    try:
        for index, upstream in enumerate(upstreams):
            server = ThreadingHTTPServer(
                ("127.0.0.1", args.listen_base + index if args.listen_base else 0),
                handler_for(observer, upstream),
            )
            server.daemon_threads = False
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            servers.append(server)
            threads.append(thread)
        monitor = threading.Thread(
            target=observer.monitor, args=(args.poll_interval,), daemon=True
        )
        monitor.start()
        observer.atomic_json(
            "ready.json",
            dict(
                proxy_urls=[f"http://127.0.0.1:{s.server_port}" for s in servers],
                upstreams=upstreams,
                initial_height=observer.last,
                max_inflight=4,
                max_state_reads_per_validator=1,
            ),
        )
        observer.stop.wait()
    finally:
        observer.stop.set()
        for server in servers:
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join()
        if "monitor" in locals():
            monitor.join()
        observer.atomic_json("summary.json", observer.summary(finished=True))
        observer.events.close()


if __name__ == "__main__":
    main()
