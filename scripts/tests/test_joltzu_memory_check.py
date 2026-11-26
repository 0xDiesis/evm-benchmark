"""Focused checks for the isolated Joltzu cgroup experiment."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "bench-targets/chains/joltzu/memory-check.py"
SPEC = importlib.util.spec_from_file_location("memory_check", SCRIPT)
memory_check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(memory_check)


class JoltzuMemoryCheckTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.output = self.root / "output"
        self.output.mkdir()
        self.cgroup = self.root / "cgroup"
        self.cgroup.mkdir()
        for name, value in {
            "memory.max": str(128 * memory_check.MIB),
            "memory.current": "32768",
            "memory.swap.max": "0",
            "memory.swap.current": "0",
            "memory.events": "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n",
            "memory.stat": "anon 8192\nfile 8192\n",
            "cpu.stat": "usage_usec 769\nuser_usec 321\n",
        }.items():
            (self.cgroup / name).write_text(
                value + "\n" if not value.endswith("\n") else value
            )
        self.args = SimpleNamespace(
            out=self.output,
            cpus=2,
            timeout=45,
            node_repo=self.root / "node checkout",
            node_build=self.root / "release",
            cast=self.root / "cast",
            solc=self.root / "solc",
            openssl="/opt/openssl/bin/openssl",
        )

    def test_container_is_networkless_and_mounts_only_output_writable(self):
        command = memory_check.container_command(
            self.args, "sha256:example", "memory-test", 128, self.output
        )
        for option in (
            "--network=none",
            "--memory=128m",
            "--memory-swap=128m",
            "--cpus=2",
            "--cgroupns=private",
            "--read-only",
            "--pull=never",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--init",
            "--entrypoint=timeout",
            "--kill-after=10s",
            "45s",
        ):
            self.assertIn(option, command)
        mounts = [command[i + 1] for i, arg in enumerate(command) if arg == "--mount"]
        self.assertEqual(
            [mount for mount in mounts if not mount.endswith(",readonly")],
            [f"type=bind,src={self.output},dst=/output"],
        )
        self.assertTrue(any("node checkout/tools/devnet.sh" in name for name in mounts))

    def test_enforces_actual_linux_limits_and_optional_memory_peak(self):
        memory_check.check_limits(self.cgroup, 128)
        sample = memory_check.sample_memory(self.cgroup)
        self.assertEqual(sample["current_bytes"], 32768)
        self.assertIsNone(sample["peak_bytes"])
        self.assertEqual(sample["resident_anon_bytes"], 8192)
        self.assertEqual(sample["cpu_usec"], 769)
        self.assertEqual(sample["events"]["max"], 0)
        with self.assertRaisesRegex(ValueError, "private memory cap"):
            memory_check.check_limits(self.cgroup, 256)
        (self.cgroup / "memory.swap.max").write_text("max\n")
        with self.assertRaisesRegex(ValueError, "zero swap"):
            memory_check.check_limits(self.cgroup, 128)

    def test_checks_finality_everywhere_before_and_after_restarting_same_state(self):
        with (
            patch.object(memory_check, "CGROUP", self.cgroup),
            patch.object(memory_check, "OUTPUT", self.output),
            patch.object(memory_check.subprocess, "run") as run,
        ):
            self.assertEqual(memory_check.worker(128, self.args.openssl), 0)
        calls = run.call_args_list
        self.assertEqual(
            [calls[i].args[0][-1] for i in (0, 2, 3, 5)], ["up", "down", "up", "down"]
        )
        commands = [call.args[0] for call in calls]
        scratch = {call.kwargs["env"]["JOLTZU_DEVNET_DIR"] for call in calls}
        self.assertEqual(scratch, {"/output/run/devnet"})
        self.assertEqual(commands[1].count("--rpc"), 5)
        self.assertEqual(commands[4].count("--rpc"), 5)
        self.assertEqual(commands[1][2:12], commands[4][2:12])
        self.assertIn("--solc", commands[1])
        self.assertNotIn("--solc", commands[4])
        self.assertEqual(
            commands[4][commands[4].index("--verify") + 1],
            str(self.output / "smoke.json"),
        )
        self.assertEqual(
            json.loads((self.output / "profile.json").read_text())["status"], "passed"
        )

    def test_failed_smoke_stops_without_restarting(self):
        failure = subprocess.CalledProcessError(1, "smoke")
        with (
            patch.object(memory_check, "CGROUP", self.cgroup),
            patch.object(memory_check, "OUTPUT", self.output),
            patch.object(
                memory_check.subprocess, "run", side_effect=[None, failure, None]
            ) as run,
        ):
            self.assertEqual(memory_check.worker(128, self.args.openssl), 1)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(run.call_args.args[0][-1], "down")

    def test_hashes_exact_input_bytes(self):
        payload = self.root / "tool"
        payload.write_bytes(b"abc")
        self.assertEqual(
            memory_check.sha256(payload),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )

    def test_requires_cap_events_for_pressure_and_distinguishes_oom(self):
        initial = {"max": 2, "oom": 1, "oom_kill": 0}
        final = dict(initial)
        profile = {
            "status": "passed",
            "before": {"events": initial},
            "stages": {"stop": {"memory": {"events": final}}},
        }
        self.assertEqual(memory_check.assess(profile, False), "passed")
        self.assertEqual(memory_check.assess(profile, True), "cap-not-reached")
        final["max"] += 1
        self.assertEqual(memory_check.assess(profile, False), "baseline-reached-cap")
        self.assertEqual(memory_check.assess(profile, True), "passed")
        final["oom"] += 1
        profile["status"] = "failed"
        self.assertEqual(memory_check.assess(profile, True), "oom")

    def test_timeout_forces_cleanup_of_the_named_container(self):
        with patch.object(
            memory_check.subprocess,
            "run",
            side_effect=[subprocess.TimeoutExpired("docker", 45), None],
        ) as run:
            self.assertEqual(
                memory_check.run_profile(self.args, "sha256:example", "baseline", 128),
                "timeout",
            )
        invocation, cleanup = [call.args[0] for call in run.call_args_list]
        container_name = next(
            argument.split("=", 1)[1]
            for argument in invocation
            if argument.startswith("--name=")
        )
        self.assertEqual(cleanup, ["docker", "rm", "-f", container_name])


if __name__ == "__main__":
    unittest.main()
