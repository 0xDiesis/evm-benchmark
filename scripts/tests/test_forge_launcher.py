"""Executable selection, nested-tool routing and cache environment regressions."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest

LAUNCHER = Path(
    os.environ.get(
        "FORGE_LAUNCHER", Path(__file__).resolve().parents[2] / "scripts/forge.sh"
    )
)
REPO_ROOT = Path(__file__).resolve().parents[2]
FORGEDS_REVISION = "bb92ff96b819bcea5ced55228d122ac725e9ffd9"
RUST_TOOLCHAIN_ACTION = "02cb101ec7c40f2c49e1d9714d64511d8e1b74de"
RUST_CACHE_ACTION = "d1527c8f67ef9af357290475d4af4b0fab8f12e0"


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

    def test_prefers_forgeds_and_preserves_arguments_and_cache_environment(self):
        ds = self.tool("forgeds")
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
        self.tool("forgeds")
        self.env["FORGE_BIN"] = str(chosen)
        result = self.run_tool("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[0][0], str(chosen))

    def test_missing_override_does_not_fallback(self):
        self.tool("forgeds")
        self.env["FORGE_BIN"] = str(self.bin / "missing")
        result = self.run_tool("build")
        self.assertEqual(result.returncode, 127)
        self.assertFalse(result.stdout)

    def test_failing_forgeds_is_not_retried_with_stock(self):
        self.tool("forgeds", 42)
        self.tool("forge")
        result = self.run_tool("build")
        self.assertEqual(result.returncode, 42)
        self.assertEqual(len(result.stdout.splitlines()), 1)

    def test_artifact_paths_match_consumers_and_allow_overrides(self):
        self.tool("forgeds")
        read_paths = 'printf "%s\\n%s\\n" "$FOUNDRY_OUT" "$FOUNDRY_CACHE_PATH"'
        result = self.run_tool("--exec", "/bin/sh", "-c", read_paths)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["out", "cache"])
        self.env.update(FOUNDRY_OUT="custom out", FOUNDRY_CACHE_PATH="custom cache")
        result = self.run_tool("--exec", "/bin/sh", "-c", read_paths)
        self.assertEqual(result.stdout.splitlines(), ["custom out", "custom cache"])

    def test_nested_tool_forge_calls_use_selected_binary(self):
        ds = self.tool("forgeds")
        self.tool("forge")
        result = self.run_tool("--exec", "/bin/sh", "-c", "forge config --json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[0], [str(ds), "config", "--json"])


    def install_prebuilt(self, revision=None, valid_checksum=True, runner_os="Linux"):
        windows = runner_os == "Windows"
        platform = "win32_amd64" if windows else "linux_amd64"
        executable = "forgeds.exe" if windows else "forgeds"
        archive_name = f"forgeds-{FORGEDS_REVISION[:7]}_{platform}.tar.gz"
        release = self.bin / "release"
        release.mkdir()
        payload = f"#!/bin/sh\nprintf '%s\\n' 'Commit SHA: {revision or FORGEDS_REVISION}'\n".encode()
        archive = release / archive_name
        with tarfile.open(archive, "w:gz") as package:
            entry = tarfile.TarInfo(executable)
            entry.mode = 0o755
            entry.size = len(payload)
            package.addfile(entry, io.BytesIO(payload))
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest() if valid_checksum else "0" * 64
        (release / (archive_name + ".sha256")).write_text(f"{checksum}  {archive_name}\n")
        curl = self.bin / "curl"
        curl.write_text(
            "#!" + sys.executable + "\nimport pathlib, shutil, sys\n"
            f"source = pathlib.Path({str(release)!r}) / sys.argv[-1].rsplit('/', 1)[-1]\n"
            "shutil.copyfile(source, sys.argv[sys.argv.index('-o') + 1])\n"
        )
        curl.chmod(0o755)
        output = self.bin / "github-output"
        output.write_text("")
        env = {
            **self.env,
            "HOME": str(self.bin / "home"),
            "RUNNER_OS": runner_os,
            "RUNNER_ARCH": "X64",
            "FORGEDS_REVISION": FORGEDS_REVISION,
            "GITHUB_OUTPUT": str(output),
            "GITHUB_PATH": str(self.bin / "github-path"),
        }
        action = (REPO_ROOT / ".github/actions/setup-forgeds/action.yml").read_text()
        step = action.split("    - name: Install prebuilt forgeds", 1)[1].split(
            "    - name: Setup pinned Rust", 1
        )[0]
        command = textwrap.dedent(step.split("      run: |\n", 1)[1])
        result = subprocess.run(["/bin/bash", "-c", command], env=env, text=True, capture_output=True)
        if result.returncode == 0 and output.read_text() == "installed=true\n":
            verify = action.split("    - name: Verify forgeds revision", 1)[1].split(
                "    - name: Cache compiler results", 1
            )[0]
            command = textwrap.dedent(verify.split("      run: |\n", 1)[1])
            result = subprocess.run(["/bin/bash", "-c", command], env=env, text=True, capture_output=True)
        return result, self.bin / "home/.cargo/bin" / executable, output.read_text()

    def test_prebuilt_installs_verified_forgeds(self):
        result, installed, output = self.install_prebuilt()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(installed.is_file())
        self.assertEqual(output, "installed=true\n")
        version = subprocess.check_output([str(installed), "--version"], text=True)
        self.assertEqual(version, f"Commit SHA: {FORGEDS_REVISION}\n")

    def test_prebuilt_installs_and_verifies_windows_executable(self):
        result, installed, output = self.install_prebuilt(runner_os="Windows")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(installed.name, "forgeds.exe")
        self.assertTrue(installed.is_file())
        self.assertEqual(output, "installed=true\n")
        self.assertIn(f"Commit SHA: {FORGEDS_REVISION}", result.stdout)

    def test_prebuilt_rejects_bad_checksum(self):
        result, installed, output = self.install_prebuilt(valid_checksum=False)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(installed.exists())
        self.assertEqual(output, "")

    def test_prebuilt_wrong_revision_requests_source_fallback(self):
        result, installed, output = self.install_prebuilt(revision="f" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(installed.exists())
        self.assertEqual(output, "")

    def test_ci_provisions_the_exact_forgeds_source_revision(self):
        action = (
            REPO_ROOT / ".github/actions/setup-forgeds/action.yml"
        ).read_text()
        workflow = (REPO_ROOT / ".github/workflows/coverage.yml").read_text()
        # The pinned revision is declared once, as the action input default.
        self.assertIn(f"    default: {FORGEDS_REVISION}\n", action)
        self.assertEqual(action.count(FORGEDS_REVISION), 1)
        self.assertIn("rust-1.99.1-${{ inputs.revision }}", action)
        self.assertIn(RUST_TOOLCHAIN_ACTION, action)
        self.assertIn(RUST_CACHE_ACTION, action)
        self.assertIn("toolchain: 1.99.1", action)
        self.assertIn("cargo +1.99.1 install", action)
        self.assertIn("-rust-1.99.1", action)
        self.assertIn("--path .forgeds-source/crates/forge --locked", action)
        self.assertIn("--bin forgeds --no-default-features", action)
        self.assertIn('features="forgeds,asm-keccak,optimism"', action)
        self.assertIn('RUSTC_WRAPPER: ""', action)
        self.assertIn("workspaces: .forgeds-source -> target", action)
        self.assertIn("ref: ${{ inputs.revision }}", action)
        self.assertIn("key: forgeds-${{ runner.os }}-${{ runner.arch }}", action)
        self.assertNotIn("SCCACHE_GHA_ENABLED", action)
        self.assertIn("path: ~/.foundry/cache/compilers", action)
        self.assertIn("forgeds-compilers-${{ runner.os }}-${{ runner.arch }}-${{ inputs.revision }}-", action)
        # The prebuilt release binary is used only after checksum and commit checks.
        self.assertIn("releases/download/forgeds\"", action)
        self.assertIn("forgeds-${FORGEDS_REVISION:0:7}_${platform}.tar.gz", action)
        self.assertIn("forgeds archive checksum mismatch", action)
        self.assertIn('grep -qF "Commit SHA: $FORGEDS_REVISION"', action)
        self.assertEqual(
            workflow.count("uses: ./.github/actions/setup-forgeds"), 1
        )
        self.assertIn(
            "python3 -m unittest scripts.tests.test_forge_launcher", workflow
        )
        self.assertGreater(
            workflow.index("uses: ./.github/actions/setup-forgeds"),
            workflow.index("- name: Generate Rust LCOV"),
        )
        self.assertIn("--instrumented", workflow)


if __name__ == "__main__":
    unittest.main()
