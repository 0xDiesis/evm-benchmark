#!/usr/bin/env python3
"""The Diesis external-fleet mode never manages the fleet, and the chain ID comes from eth_chainId."""
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import subprocess
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
LIB = ROOT / "bench-targets" / "scripts" / "lib.sh"


class ChainIdHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        result = "0x1e240" if request["method"] == "eth_chainId" else "0x1"
        body = json.dumps(dict(jsonrpc="2.0", id=request["id"], result=result)).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def chain_config(env):
    script = (f'source "{LIB}" && load_chain_config diesis && '
              'printf "%s\\n" "$CHAIN_CHAIN_ID" "$CHAIN_UP_CMD" "$CHAIN_DOWN_CMD" "$CHAIN_CLEAN_CMD" "$CHAIN_REBUILD_CMD"')
    out = subprocess.run(["bash", "-c", script], env={"PATH": "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin", **env},
                         capture_output=True, text=True)
    return out.returncode, out.stdout.split("\n")[:5], out.stderr


class ExternalFleet(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), ChainIdHandler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def test_external_mode_reads_chain_id_and_never_manages_the_fleet(self):
        code, (chain_id, up, down, clean, rebuild), _ = chain_config(
            {"DIESIS_FLEET": "external", "DIESIS_RPC": self.url, "DIESIS_REPO_DIR": "/nonexistent"})
        self.assertEqual(code, 0)
        self.assertEqual(chain_id, "123456")
        self.assertEqual((up, down, clean, rebuild), (":", ":", "", ""))

    def test_explicit_chain_id_wins(self):
        _, (chain_id, *_), _ = chain_config({"DIESIS_FLEET": "external", "DIESIS_RPC": self.url,
                                             "DIESIS_CHAIN_ID": "42"})
        self.assertEqual(chain_id, "42")

    def test_unreachable_managed_cluster_falls_back_to_e2e_chain_id(self):
        code, (chain_id, up, *_), _ = chain_config({"DIESIS_RPC": "http://127.0.0.1:9"})
        self.assertEqual((code, chain_id), (0, "19803"))
        self.assertNotEqual(up, ":")

    def test_unknown_fleet_mode_is_rejected(self):
        code, _, _ = chain_config({"DIESIS_FLEET": "remote", "DIESIS_RPC": self.url})
        self.assertNotEqual(code, 0)


if __name__ == "__main__":
    unittest.main()
