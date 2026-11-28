"""Joltzu target registration and safe devnet ownership boundaries."""

from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "bench-targets/chains/joltzu/devnet.py"
METRICS = LAUNCHER.with_name("metrics.py")


class JoltzuTargetTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "source with spaces"
        (self.repo / "tools").mkdir(parents=True)
        self.calls = self.root / "calls.jsonl"
        script = self.repo / "tools/devnet.sh"
        script.write_text(
            "#!" + sys.executable + "\n"
            "import json, os, pathlib, sys\n"
            "with open(os.environ['CALLS'], 'a') as f: "
            "f.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "p=pathlib.Path(sys.argv[2])\n"
            "if sys.argv[1]=='init':\n"
            " p.mkdir(); (p/'devnet.env').write_text('validators=4\\n')\n"
            "if sys.argv[1]=='clean':\n"
            " import shutil; shutil.rmtree(p)\n"
        )
        script.chmod(0o755)
        self.data = self.root / "data with spaces"
        self.env = dict(
            os.environ,
            JOLTZU_REPO_DIR=str(self.repo),
            JOLTZU_DEVNET_DIR=str(self.data),
            CALLS=str(self.calls),
        )
        self.env.pop("JOLTZU_PROMETHEUS", None)

    def run_target(self, command):
        return subprocess.run(
            [sys.executable, str(LAUNCHER), command],
            env=self.env,
            text=True,
            capture_output=True,
        )

    def recorded_calls(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def test_registered_with_devnet_endpoints_and_batch_limit(self):
        result = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; load_chain_config joltzu; '
                'printf "%s\\n" "$CHAIN_RPC" "$CHAIN_WS" "$CHAIN_CHAIN_ID" '
                '"$CHAIN_MAX_BATCH_SIZE" "$CHAIN_EXPECT_EMPTY_BLOCKS"; '
                "limit_chain_batch_size 200; limit_chain_batch_size 4; "
                "load_chain_config anvil; "
                'printf "%s\\n" "${CHAIN_MAX_BATCH_SIZE:-unset}" "$CHAIN_EXPECT_EMPTY_BLOCKS"; '
                "limit_chain_batch_size 200",
                "_",
                str(ROOT / "bench-targets/scripts/lib.sh"),
            ],
            env=self.env,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            [
                "http://127.0.0.1:30400",
                "ws://127.0.0.1:30400",
                "31337",
                "16",
                "false",
                "16",
                "4",
                "unset",
                "true",
                "200",
            ],
        )

    def test_up_initializes_four_validators_and_resumes_owned_network(self):
        first = self.run_target("up")
        self.assertEqual(first.returncode, 0, first.stderr)
        second = self.run_target("up")
        self.assertEqual(second.returncode, 0, second.stderr)
        calls = self.recorded_calls()
        self.assertEqual([call[0] for call in calls], ["init", "start", "start"])
        self.assertTrue(all(call[2:] == [] for call in calls[1:]))
        self.assertEqual(
            calls[0],
            [
                "init",
                str(self.data),
                "--validators",
                "4",
                "--chain-id",
                "31337",
                "--base-port",
                "30300",
            ],
        )

    def test_reset_cleans_only_owned_network_then_reinitializes(self):
        self.assertEqual(self.run_target("up").returncode, 0)
        result = self.run_target("reset")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [x[0] for x in self.recorded_calls()],
            ["init", "start", "clean", "init", "start"],
        )

    def test_prometheus_is_opt_in_and_survives_reset(self):
        self.env["JOLTZU_PROMETHEUS"] = "1"
        self.assertEqual(self.run_target("up").returncode, 0)
        self.assertEqual(self.run_target("reset").returncode, 0)
        self.assertEqual(
            [call[2:] for call in self.recorded_calls() if call[0] == "start"],
            [["--prometheus"], ["--prometheus"]],
        )

    def run_failing_benchmark(self, output=None):
        commands = self.root / "commands"
        commands.mkdir()
        target = self.root / "target"
        (target / "release").mkdir(parents=True)
        harness = target / "release" / "evm-benchmark"
        harness.write_text("#!/bin/sh\nexit 17\n")
        harness.chmod(0o755)
        metadata = shlex.quote(json.dumps({"target_directory": str(target)}))
        for name, body in {
            "cargo": f'printf "%s\\n" {metadata}\n',
            "curl": "exit 0\n",
            "python3": (
                'case "$1:$2" in\n'
                '  */joltzu/metrics.py:--out) printf "{}\\n" > "$3"; exit ;;\n'
                '  */prometheus-diagnostics.py:snapshot) printf "{}\\n" > "$6"; exit ;;\n'
                "esac\n"
                f'exec {shlex.quote(sys.executable)} "$@"\n'
            ),
        }.items():
            command = commands / name
            command.write_text(f"#!/bin/sh\n{body}")
            command.chmod(0o755)
        args = [
            "bash", str(ROOT / "bench-targets/scripts/bench.sh"),
            "--chain", "joltzu", "--mode", "burst", "--no-fund",
        ]
        if output is not None:
            args.extend(("--out", str(output)))
        return subprocess.run(
            args,
            env=dict(
                self.env,
                PATH=f"{commands}:{os.environ['PATH']}",
                BENCH_RESULTS_BASE=str(self.root / "results"),
                BENCH_PROMETHEUS_URL="http://127.0.0.1:9090",
                BENCH_PROMETHEUS_CAPTURE="true",
                BENCH_PROMETHEUS_AFTER_SETTLE_SECS="0",
                JOLTZU_PROMETHEUS="1",
            ),
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_failed_benchmark_keeps_snapshots_without_publishing_old_report(self):
        results = self.root / "results"
        runs = results / "runs"
        chain = runs / "joltzu"
        mode = chain / "burst"
        output = mode / "reused"
        output.mkdir(parents=True)
        pointers = {
            mode / "latest": "previous",
            chain / "latest": "sustained",
            runs / "latest": "diesis",
        }
        for link, destination in pointers.items():
            (link.parent / destination).mkdir()
            link.symlink_to(destination, target_is_directory=True)
        index = results / "index.json"
        prior_index = json.dumps([{"path": str(mode / "previous")}]) + "\n"
        index.write_text(prior_index)
        report = output / "report.json"
        prior_report = json.dumps({"results": {
            "confirmed": 1,
            "confirmed_tps": 1,
            "latency": {"p50": 1, "p95": 1, "p99": 1},
        }}) + "\n"
        report.write_text(prior_report)

        result = self.run_failing_benchmark(output)
        self.assertEqual(result.returncode, 17, result.stdout + result.stderr)
        for phase in ("before", "after"):
            self.assertTrue((output / f"joltzu-metrics-{phase}.json").is_file())
            self.assertTrue((output / f"prometheus-{phase}.json").is_file())
        self.assertTrue((output / "prometheus-delta.json").is_file())
        for link, destination in pointers.items():
            with self.subTest(link=link):
                self.assertEqual(os.readlink(link), destination)
        self.assertEqual(index.read_text(), prior_index)
        self.assertEqual(report.read_text(), prior_report)

    def test_failed_benchmark_with_default_output_keeps_latest(self):
        mode = self.root / "results/runs/joltzu/burst"
        (mode / "previous").mkdir(parents=True)
        latest = mode / "latest"
        latest.symlink_to("previous", target_is_directory=True)

        result = self.run_failing_benchmark()
        self.assertEqual(result.returncode, 17, result.stdout + result.stderr)
        self.assertEqual(os.readlink(latest), "previous")

    def test_refuses_existing_unowned_directory(self):
        self.data.mkdir()
        keep = self.data / "keep"
        keep.write_text("unrelated")
        for command in ["up", "down", "reset"]:
            result = self.run_target(command)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("ownership", result.stderr)
        self.assertEqual(keep.read_text(), "unrelated")
        self.assertFalse(self.calls.exists())

    def test_refuses_symlink_data_path(self):
        actual = self.root / "actual"
        actual.mkdir()
        self.data.symlink_to(actual, target_is_directory=True)
        result = self.run_target("reset")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink", result.stderr)
        self.assertTrue(actual.is_dir())
        self.assertFalse(self.calls.exists())

    def test_refuses_source_directory_as_data(self):
        self.env["JOLTZU_DEVNET_DIR"] = str(self.repo)
        result = self.run_target("reset")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("source", result.stderr)
        self.assertTrue((self.repo / "tools/devnet.sh").exists())


class JoltzuMetricsTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("joltzu_metrics", METRICS)
        self.metrics = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.metrics)

    def test_scrapes_bounded_http_response_and_keeps_exact_counters(self):
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200 if self.path == "/metrics" else 404)
                self.end_headers()
                self.wfile.write(
                    b"# TYPE joltzu_finalized_height gauge\n"
                    b"joltzu_finalized_height 12\n"
                    b"joltzu_mailbox_dropped_frames_total 9007199254740993\n"
                    b"joltzu_peer_send_timeouts_total 0\n"
                    b"joltzu_signer_seconds_total 0.0012\n"
                    b'unrelated{ignored="true"} 1\n'
                )

            def log_message(self, *_):
                pass

        with HTTPServer(("127.0.0.1", 0), Handler) as server:
            server.timeout = 2
            worker = threading.Thread(target=server.handle_request)
            worker.start()
            try:
                samples = self.metrics.scrape(server.server_port)
            finally:
                worker.join(timeout=3)
        self.assertEqual(samples["joltzu_finalized_height"], 12)
        self.assertEqual(samples["joltzu_mailbox_dropped_frames_total"], 9007199254740993)
        self.assertEqual(samples["joltzu_peer_send_timeouts_total"], 0)
        self.assertEqual(samples["joltzu_signer_seconds_total"], 0.0012)
        self.assertNotIn("unrelated", samples)

    def test_required_metrics_are_not_replaced_with_zero(self):
        with self.assertRaisesRegex(ValueError, "missing.*finalized_height"):
            self.metrics.parse_samples("joltzu_peer_send_timeouts_total 0\n")

    def test_rejects_bad_status_or_oversized_response(self):
        with patch.object(self.metrics, "HTTPConnection") as connection:
            response = connection.return_value.getresponse.return_value.__enter__.return_value
            response.status = 302
            with self.assertRaisesRegex(ValueError, "HTTP 302"):
                self.metrics.scrape(30400)
            response.read.assert_not_called()
            response.status = 200
            response.read.return_value = b"x" * (self.metrics.MAX_RESPONSE_BYTES + 1)
            with self.assertRaisesRegex(ValueError, "size limit"):
                self.metrics.scrape(30400)
            response.read.assert_called_once_with(self.metrics.MAX_RESPONSE_BYTES + 1)
            connection.return_value.close.assert_called()

    def test_invalid_and_duplicate_samples_are_rejected(self):
        for text in (
            "joltzu_finalized_height 1\njoltzu_finalized_height 2\n",
            "joltzu_finalized_height 1.5\n",
            "joltzu_signer_seconds_total NaN\n",
            "joltzu_durable_commit_seconds_total -1\n",
        ):
            with self.subTest(text=text), self.assertRaisesRegex(ValueError, "sample"):
                self.metrics.parse_samples(text)

    def test_partial_scrape_records_node_errors_and_custom_ports(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "metrics.json"
            with patch.object(
                self.metrics,
                "scrape",
                side_effect=[{"height": 1}, OSError("refused"), {}, {}, {}],
            ) as scrape:
                self.assertEqual(self.metrics.main(["--base-port", "31000", "--out", str(output)]), 1)
            nodes = json.loads(output.read_text())["nodes"]
        self.assertEqual([call.args[0] for call in scrape.call_args_list], list(range(31100, 31105)))
        self.assertEqual(nodes["observer"]["samples"], {"height": 1})
        self.assertEqual(nodes["node1"]["error"], "refused")
        self.assertIn("http://127.0.0.1:31104/metrics", nodes["node4"]["url"])


if __name__ == "__main__":
    unittest.main()
