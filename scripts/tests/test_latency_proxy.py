"""Exercise receipt observation through real loopback HTTP servers."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from urllib.request import Request, urlopen

SCRIPT = Path(__file__).resolve().parents[2] / "bench-targets/scripts/latency-proxy.py"
TX = "0x" + "11" * 32
BLOCK = "0x" + "22" * 32


def post(url, body):
    with urlopen(
        Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"}),
        timeout=5,
    ) as response:
        return json.load(response)


class MockChain:
    def __init__(self, state=None):
        self.state = state or SimpleNamespace(included=False)
        self.finalized = False
        self.fail_send = False
        self.send_delay = 0
        self.receipt_delay = 0
        self.receipt_completed = None
        self.receipt_reads = 0
        self.max_receipt_reads = 0
        self.read_lock = threading.Lock()
        self.reverted = False
        chain = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))

                def answer(request):
                    method = request["method"]
                    result = None
                    if method == "eth_chainId":
                        result = "0x7a69"
                    elif method == "eth_sendRawTransaction":
                        if chain.fail_send:
                            return dict(
                                jsonrpc="2.0",
                                id=request["id"],
                                error={"code": -32000, "message": "rejected"},
                            )
                        chain.included = True
                        time.sleep(chain.send_delay)
                        result = TX
                    elif method == "eth_blockNumber":
                        result = "0x1" if chain.included else "0x0"
                    elif method == "eth_getBlockByNumber":
                        finalized = request["params"][0] == "finalized"
                        height = (
                            1
                            if chain.included and (not finalized or chain.finalized)
                            else 0
                        )
                        result = dict(
                            number=hex(height),
                            hash=BLOCK if height else "0x" + "00" * 32,
                            transactions=[TX] if height else [],
                        )
                    elif method == "eth_getBlockReceipts":
                        with chain.read_lock:
                            chain.receipt_reads += 1
                            chain.max_receipt_reads = max(
                                chain.max_receipt_reads, chain.receipt_reads
                            )
                        try:
                            time.sleep(chain.receipt_delay)
                        finally:
                            with chain.read_lock:
                                chain.receipt_reads -= 1
                        result = (
                            [
                                dict(
                                    transactionHash=TX,
                                    blockNumber="0x1",
                                    blockHash=BLOCK,
                                    status="0x0" if chain.reverted else "0x1",
                                )
                            ]
                            if chain.included
                            else []
                        )
                        chain.receipt_completed = time.monotonic_ns()
                    return dict(jsonrpc="2.0", id=request["id"], result=result)

                response = (
                    [answer(item) for item in body]
                    if isinstance(body, list)
                    else answer(body)
                )
                encoded = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    @property
    def included(self):
        return self.state.included

    @included.setter
    def included(self, value):
        self.state.included = value

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class LatencyProxyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.chain = MockChain()
        self.addCleanup(self.chain.close)
        self.chains = [self.chain]
        for _ in range(3):
            chain = MockChain(self.chain.state)
            self.chains.append(chain)
            self.addCleanup(chain.close)
        self.out = self.root / "observe"
        self.process = subprocess.Popen(
            [
                sys.executable,
                str(SCRIPT),
                "--upstreams",
                ",".join(chain.url for chain in self.chains),
                "--listen-base",
                "0",
                "--out",
                str(self.out),
                "--chain-id",
                "31337",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(self.stop)
        self.wait_for(lambda: (self.out / "ready.json").exists())
        self.urls = json.loads((self.out / "ready.json").read_text())["proxy_urls"]

    def wait_for(self, predicate, seconds=5):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if predicate():
                return
            if self.process.poll() is not None:
                self.fail("Proxy exited: " + self.process.communicate()[1])
            time.sleep(0.01)
        self.fail("Observation did not complete before deadline")

    def events(self):
        path = self.out / "events.jsonl"
        return (
            [json.loads(line) for line in path.read_text().splitlines()]
            if path.exists()
            else []
        )

    def send(self):
        return post(
            self.urls[0],
            [
                dict(
                    jsonrpc="2.0",
                    id=1,
                    method="eth_sendRawTransaction",
                    params=["0x00"],
                )
            ],
        )[0]

    def stop(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
        self.process.communicate(timeout=15)

    def summary(self):
        self.stop()
        return json.loads((self.out / "summary.json").read_text())

    def test_early_receipt_survives_delayed_submission_response(self):
        self.chain.send_delay = 0.25
        for chain in self.chains:
            chain.finalized = True
        self.assertEqual(self.send()["result"], TX)
        self.wait_for(lambda: any(e["event"] == "finalized" for e in self.events()))
        result = self.summary()
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["accepted"], 1)
        self.assertLess(
            result["receipt_latency_ms"]["p50"], result["submission_latency_ms"]["p50"]
        )

    def test_receipt_timestamp_is_after_delayed_read_and_finality_waits_for_every_validator(
        self,
    ):
        self.chain.receipt_delay = 0.15
        for chain in self.chains[:3]:
            chain.finalized = True
        self.send()
        self.wait_for(lambda: any(e["event"] == "receipt" for e in self.events()))
        receipt = next(e for e in self.events() if e["event"] == "receipt")
        self.assertGreaterEqual(receipt["completed_ns"], self.chain.receipt_completed)
        self.assertGreaterEqual(receipt["latency_ms"], 140)
        self.assertFalse(any(e["event"] == "finalized" for e in self.events()))
        self.chains[-1].finalized = True
        self.wait_for(lambda: any(e["event"] == "finalized" for e in self.events()))
        result = self.summary()
        self.assertEqual(result["status"], "passed")
        self.assertGreaterEqual(
            result["finality_latency_ms"]["p50"], result["receipt_latency_ms"]["p50"]
        )

    def test_failed_submission_is_forwarded_and_never_counted_as_accepted(self):
        self.chain.fail_send = True
        self.assertEqual(self.send()["error"]["message"], "rejected")
        result = self.summary()
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["accepted"], 0)
        self.assertEqual(result["failed_submissions"], 1)

    def test_revert_and_missing_finality_fail_on_graceful_shutdown(self):
        self.chain.reverted = True
        self.send()
        self.wait_for(lambda: any(e["event"] == "receipt" for e in self.events()))
        result = self.summary()
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["reverted"], 1)
        self.assertEqual(result["missing_finality"], 1)
        self.assertFalse((self.out / "summary.json.tmp").exists())

    def test_duplicate_hash_is_counted_once(self):
        for chain in self.chains:
            chain.finalized = True
        self.send()
        self.send()
        self.wait_for(lambda: any(e["event"] == "finalized" for e in self.events()))
        result = self.summary()
        self.assertEqual(result["accepted"], 1)
        self.assertEqual(result["duplicate_submissions"], 1)
        self.assertEqual(result["receipt_latency_ms"]["count"], 1)
        self.assertEqual(result["status"], "failed")

    def test_transport_records_queue_and_upstream_time_separately(self):
        self.chain.send_delay = 0.05
        self.send()
        self.wait_for(
            lambda: any(
                e["event"] == "rpc_transport"
                and "eth_sendRawTransaction" in e["methods"]
                for e in self.events()
            )
        )
        event = next(
            e
            for e in self.events()
            if e["event"] == "rpc_transport"
            and "eth_sendRawTransaction" in e["methods"]
        )
        self.assertLessEqual(event["queued_ns"], event["started_ns"])
        self.assertLessEqual(event["started_ns"], event["completed_ns"])
        self.assertGreaterEqual(event["queue_ms"], 0)
        self.assertGreaterEqual(event["upstream_ms"], 50)

    def test_state_reads_on_one_validator_do_not_compete_for_its_single_read_slot(self):
        self.chain.receipt_delay = 0.1
        body = dict(jsonrpc="2.0", id=1, method="eth_getBlockReceipts", params=["0x1"])
        with ThreadPoolExecutor(max_workers=2) as pool:
            requests = [pool.submit(post, self.urls[0], body) for _ in range(2)]
            for request in requests:
                self.assertEqual(request.result()["result"], [])
        self.assertEqual(self.chain.max_receipt_reads, 1)


if __name__ == "__main__":
    unittest.main()
