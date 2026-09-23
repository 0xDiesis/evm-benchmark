"""Executable selection, nested-tool routing and cache environment regressions."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

LAUNCHER = Path(
    os.environ.get(
        "FORGE_LAUNCHER", Path(__file__).resolve().parents[2] / "scripts/forge.sh"
    )
)
REPO_ROOT = Path(__file__).resolve().parents[2]
FORGE_DS_REVISION = "6975c33c48e438d986fe90c7d6c0c5502f1235ed"
RUST_TOOLCHAIN_ACTION = "02cb101ec7c40f2c49e1d9714d64511d8e1b74de"
SCCACHE_ACTION = "fc920bf0ec8de6ee65d409111f7ec508035751ba"


class ForgeLauncherTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.bin = Path(self.temp.name)
        self.env = dict(
            os.environ,
            PATH=str(self.bin) + ":/usr/bin:/bin",
            RUSTC_WRAPPER="cache wrapper",
            SCCACHE_DIR="cache path",
        )
        self.env.pop("FORGE_BIN", None)
        self.env.pop("DIESIS_FORGE_EXECUTABLE", None)
        self.env.pop("FOUNDRY_OUT", None)
        self.env.pop("FOUNDRY_CACHE_PATH", None)

    def tool(self, name, code=0):
        path = self.bin / name
        path.write_text(
            "#!" + sys.executable + "\n"
            "import json, os, sys\n"
            "print(json.dumps([sys.argv, os.environ.get('RUSTC_WRAPPER'), "
            "os.environ.get('SCCACHE_DIR')]))\n"
            f"sys.exit({code})\n"
        )
        path.chmod(0o755)
        return path

    def run_tool(self, *args):
        return subprocess.run(
            ["/bin/sh", str(LAUNCHER), *args],
            env=self.env,
            text=True,
            capture_output=True,
        )

    def test_prefers_ds_and_preserves_arguments_and_cache_environment(self):
        ds = self.tool("forge-ds")
        self.tool("forge")
        result = self.run_tool("test", "--match-test", "a b", "literal;$HOME")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout),
            [
                [str(ds), "test", "--match-test", "a b", "literal;$HOME"],
                "cache wrapper",
                "cache path",
            ],
        )

    def test_standard_forge_fallback(self):
        stock = self.tool("forge")
        result = self.run_tool("--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[0], [str(stock), "--version"])

    def test_explicit_override_with_spaces(self):
        chosen = self.tool("chosen forge")
        self.tool("forge-ds")
        self.env["FORGE_BIN"] = str(chosen)
        result = self.run_tool("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[0][0], str(chosen))

    def test_missing_override_does_not_fallback(self):
        self.tool("forge-ds")
        self.env["FORGE_BIN"] = str(self.bin / "missing")
        result = self.run_tool("build")
        self.assertEqual(result.returncode, 127)
        self.assertFalse(result.stdout)

    def test_failing_ds_is_not_retried_with_stock(self):
        self.tool("forge-ds", 42)
        self.tool("forge")
        result = self.run_tool("build")
        self.assertEqual(result.returncode, 42)
        self.assertEqual(len(result.stdout.splitlines()), 1)

    def test_artifact_paths_match_consumers_and_allow_overrides(self):
        self.tool("forge-ds")
        read_paths = 'printf "%s\\n%s\\n" "$FOUNDRY_OUT" "$FOUNDRY_CACHE_PATH"'
        result = self.run_tool("--exec", "/bin/sh", "-c", read_paths)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["out", "cache"])
        self.env.update(FOUNDRY_OUT="custom out", FOUNDRY_CACHE_PATH="custom cache")
        result = self.run_tool("--exec", "/bin/sh", "-c", read_paths)
        self.assertEqual(result.stdout.splitlines(), ["custom out", "custom cache"])

    def test_nested_tool_forge_calls_use_selected_binary(self):
        ds = self.tool("forge-ds")
        self.tool("forge")
        result = self.run_tool("--exec", "/bin/sh", "-c", "forge config --json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[0], [str(ds), "config", "--json"])

    def test_ci_provisions_the_exact_forge_ds_source_revision(self):
        action = (
            REPO_ROOT / ".github/actions/setup-forge-ds/action.yml"
        ).read_text()
        workflow = (REPO_ROOT / ".github/workflows/coverage.yml").read_text()
        self.assertIn(FORGE_DS_REVISION, action)
        self.assertIn(RUST_TOOLCHAIN_ACTION, action)
        self.assertIn(SCCACHE_ACTION, action)
        self.assertIn("toolchain: 1.97.1", action)
        self.assertIn("cargo +1.97.1 install", action)
        self.assertIn("-rust-1.97.1", action)
        self.assertIn("--locked --bin forge-ds --features forge-ds forge", action)
        self.assertIn("RUSTC_WRAPPER: sccache", action)
        # The prebuilt release binary is used only after checksum and commit checks.
        self.assertIn("forge-ds_${platform}.tar.gz", action)
        self.assertIn("forge-ds archive checksum mismatch", action)
        self.assertIn('grep -qF "Commit SHA: $FORGE_DS_REVISION"', action)
        self.assertEqual(
            workflow.count("uses: ./.github/actions/setup-forge-ds"), 1
        )
        self.assertIn(
            "python3 -m unittest scripts.tests.test_forge_launcher", workflow
        )
        self.assertGreater(
            workflow.index("uses: ./.github/actions/setup-forge-ds"),
            workflow.index("- name: Generate Rust LCOV"),
        )
        self.assertIn("--instrumented", workflow)


if __name__ == "__main__":
    unittest.main()
