"""Joltzu target registration and safe devnet ownership boundaries."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "bench-targets/chains/joltzu/devnet.py"


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


if __name__ == "__main__":
    unittest.main()
