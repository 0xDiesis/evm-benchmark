"""Executable selection, nested-tool routing and cache environment regressions."""

import hashlib
import io
import json
import os
from pathlib import Path
import shlex
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
FORGEDS_REVISION = "4ca15fcba25aa61a1b436d28b428e30b459f3d56"
RUST_TOOLCHAIN_ACTION = "02cb101ec7c40f2c49e1d9714d64511d8e1b74de"
RUST_CACHE_ACTION = "d1527c8f67ef9af357290475d4af4b0fab8f12e0"
REVIEWED_ARCHIVE_DIGESTS = {
    "linux_amd64": "a1341d56a7fc743b4d87853596baaf0931a6e4348106def1a6994bc793c5b62f",
    "linux_arm64": "1555c23e95f42d4c0c907181a0ce157d42e40fa8028e9026caaf524ee007d522",
    "darwin_arm64": "39c4b0fd5ab8cd7ee2b411a1e0a5ac8519a5566bdd25d50f90ae9bd47ab01f63",
    "win32_amd64": "b43253bc6fb866df17c8a567ff41ce5ef8d615bc93739317c1f4884ed9566f64",
}
REVIEWED_BINARY_DIGESTS = {
    "linux_amd64": "236083c70a100792a61206fb48aa082813671c0e5623bf11d7d0a03ea02ded5d",
    "linux_arm64": "0fc34873bed14e185fc83e585840d5c689734e251d09dd7fcd8f85bec8e96776",
    "darwin_arm64": "a94c0565fc86a3cc91b73b745bfa76dfb5dfafbaf3beaba536b36651fd9e044d",
    "win32_amd64": "fd738afbec4b6e0b8cc0f6c3718c553bcacfb0aed26ca28773b8c2b9db89fa46",
}


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


    def action_step(self, name, next_name):
        action = (REPO_ROOT / ".github/actions/setup-forgeds/action.yml").read_text()
        step = action.split(f"    - name: {name}\n", 1)[1].split(f"    - name: {next_name}", 1)[0]
        return textwrap.dedent(step.split("      run: |\n", 1)[1])

    def run_step(self, name, next_name, env):
        command = self.action_step(name, next_name)
        return subprocess.run(["/bin/bash", "-c", command], env=env, text=True, capture_output=True)

    def resolve_pins(self, runner_os, runner_arch, revision=FORGEDS_REVISION):
        output = Path(tempfile.mkdtemp(dir=self.bin)) / "github-output"
        output.write_text("")
        env = {
            **self.env,
            "RUNNER_OS": runner_os,
            "RUNNER_ARCH": runner_arch,
            "FORGEDS_REVISION": revision,
            "GITHUB_OUTPUT": str(output),
        }
        result = self.run_step(
            "Resolve reviewed forgeds digests", "Restore forgeds binary", env
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return dict(line.split("=", 1) for line in output.read_text().splitlines())

    def test_resolves_reviewed_digests_for_each_runner(self):
        for runner, platform, executable in (
            (("Linux", "X64"), "linux_amd64", "forgeds"),
            (("Linux", "ARM64"), "linux_arm64", "forgeds"),
            (("macOS", "ARM64"), "darwin_arm64", "forgeds"),
            (("Windows", "X64"), "win32_amd64", "forgeds.exe"),
        ):
            with self.subTest(platform=platform):
                self.assertEqual(
                    self.resolve_pins(*runner),
                    {
                        "platform": platform,
                        "executable": executable,
                        "archive_digest": REVIEWED_ARCHIVE_DIGESTS[platform],
                        "binary_digest": REVIEWED_BINARY_DIGESTS[platform],
                    },
                )

    def test_resolves_no_digest_for_unreviewed_revision_or_runner(self):
        pins = self.resolve_pins("Linux", "X64", revision="f" * 40)
        self.assertEqual((pins["platform"], pins["archive_digest"], pins["binary_digest"]), ("linux_amd64", "", ""))
        pins = self.resolve_pins("macOS", "X64")
        self.assertEqual((pins["platform"], pins["archive_digest"], pins["binary_digest"]), ("", "", ""))

    def fake_binary(self, path, reports=None, code=0, marker=None):
        path.parent.mkdir(parents=True, exist_ok=True)
        ran = f"touch {shlex.quote(str(marker))}\n" if marker else ""
        path.write_text(
            f"#!/bin/sh\n{ran}printf '%s\\n' 'Commit SHA: {reports or FORGEDS_REVISION}'\nexit {code}\n"
        )
        path.chmod(0o755)
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def install_prebuilt(
        self,
        reports=None,
        published="archive",
        archive_pin="fixture",
        binary_pin="fixture",
        runner_os="Linux",
        revision=FORGEDS_REVISION,
    ):
        windows = runner_os == "Windows"
        platform = "win32_amd64" if windows else "linux_amd64"
        executable = "forgeds.exe" if windows else "forgeds"
        archive_name = f"forgeds-{revision[:7]}_{platform}.tar.gz"
        release = self.bin / "release"
        release.mkdir()
        payload = self.bin / "payload" / executable
        binary_digest = self.fake_binary(payload, reports=reports or revision)
        archive = release / archive_name
        with tarfile.open(archive, "w:gz") as package:
            package.add(payload, arcname=executable)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        reviewed = {
            "fixture": digest,
            "reviewed": REVIEWED_ARCHIVE_DIGESTS[platform],
            "none": "",
        }
        checksum = {**reviewed, "archive": digest, "zeros": "0" * 64}[published]
        (release / (archive_name + ".sha256")).write_text(f"{checksum}  {archive_name}\n")
        downloads = self.bin / "downloads"
        curl = self.bin / "curl"
        curl.write_text(
            "#!" + sys.executable + "\nimport pathlib, shutil, sys\n"
            f"with open({str(downloads)!r}, 'a') as log: log.write(sys.argv[-1] + '\\n')\n"
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
            "FORGEDS_REVISION": revision,
            "PLATFORM": platform,
            "EXECUTABLE": executable,
            "ARCHIVE_DIGEST": reviewed[archive_pin],
            "BINARY_DIGEST": {
                "fixture": binary_digest,
                "reviewed": REVIEWED_BINARY_DIGESTS[platform],
                "none": "",
            }[binary_pin],
            "GITHUB_OUTPUT": str(output),
            "GITHUB_PATH": str(self.bin / "github-path"),
        }
        result = self.run_step("Install prebuilt forgeds", "Setup pinned Rust for forgeds", env)
        if result.returncode == 0 and output.read_text() == "installed=true\n":
            result = self.run_step("Verify forgeds revision", "Save reviewed forgeds binary", env)
        calls = downloads.read_text().splitlines() if downloads.exists() else []
        return result, self.bin / "home/.cargo/bin" / executable, output.read_text(), calls

    def test_prebuilt_installs_verified_forgeds(self):
        result, installed, output, _ = self.install_prebuilt()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(installed.is_file())
        self.assertEqual(output, "installed=true\n")
        version = subprocess.check_output([str(installed), "--version"], text=True)
        self.assertEqual(version, f"Commit SHA: {FORGEDS_REVISION}\n")

    def test_prebuilt_installs_and_verifies_windows_executable(self):
        result, installed, output, _ = self.install_prebuilt(runner_os="Windows")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(installed.name, "forgeds.exe")
        self.assertTrue(installed.is_file())
        self.assertEqual(output, "installed=true\n")
        self.assertIn(f"Commit SHA: {FORGEDS_REVISION}", result.stdout)

    def assert_rejected(self, message, **kwargs):
        result, installed, output, _ = self.install_prebuilt(**kwargs)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(message, result.stdout)
        self.assertFalse(installed.exists())
        self.assertEqual(output, "")

    def test_prebuilt_rejects_bad_checksum(self):
        self.assert_rejected("disagrees with the reviewed digest", published="zeros")

    def test_prebuilt_rejects_release_files_replaced_together(self):
        # A replaced archive with a matching replaced .sha256 still differs
        # from the reviewed digest in the action.
        self.assert_rejected("disagrees with the reviewed digest", archive_pin="reviewed")

    def test_prebuilt_rejects_archive_that_differs_from_reviewed_digest(self):
        self.assert_rejected(
            "forgeds archive checksum mismatch", published="reviewed", archive_pin="reviewed"
        )

    def test_prebuilt_rejects_executable_that_differs_from_reviewed_digest(self):
        self.assert_rejected("forgeds executable checksum mismatch", binary_pin="reviewed")

    def test_reviewed_archive_reporting_another_commit_fails(self):
        self.assert_rejected("does not report the pinned commit", reports="f" * 40)

    def test_revision_without_reviewed_digest_builds_from_source(self):
        result, installed, output, calls = self.install_prebuilt(
            archive_pin="none", binary_pin="none", revision="f" * 40
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("building from source", result.stdout)
        self.assertEqual(calls, [])
        self.assertFalse(installed.exists())
        self.assertEqual(output, "")

    def verify_cached(self, reports=None, code=0, pin="cached", runner_os="Linux"):
        home = Path(tempfile.mkdtemp(dir=self.bin))
        executable = "forgeds.exe" if runner_os == "Windows" else "forgeds"
        cached = home / ".cargo/bin" / executable
        marker = home / "executed"
        digest = self.fake_binary(cached, reports=reports, code=code, marker=marker)
        output = home / "github-output"
        output.write_text("")
        env = {
            **self.env,
            "HOME": str(home),
            "RUNNER_OS": runner_os,
            "FORGEDS_REVISION": FORGEDS_REVISION,
            "EXECUTABLE": executable,
            "BINARY_DIGEST": {"cached": digest, "other": "0" * 64, "none": ""}[pin],
            "GITHUB_OUTPUT": str(output),
        }
        result = self.run_step("Verify restored forgeds", "Install prebuilt forgeds", env)
        self.assertEqual(result.returncode, 0, result.stderr)
        return cached.exists(), marker.exists(), output.read_text()

    def test_cached_reviewed_forgeds_is_kept(self):
        for runner_os in ("Linux", "Windows"):
            with self.subTest(runner_os=runner_os):
                self.assertEqual(
                    self.verify_cached(runner_os=runner_os), (True, True, "verified=true\n")
                )

    def test_cached_forgeds_with_other_bytes_is_discarded_without_running(self):
        for pin in ("other", "none"):
            with self.subTest(pin=pin):
                self.assertEqual(self.verify_cached(pin=pin), (False, False, ""))

    def test_cached_reviewed_bytes_must_still_report_the_pinned_commit(self):
        for reports, code in (("f" * 40, 0), (None, 3)):
            with self.subTest(reports=reports, code=code):
                kept, _, output = self.verify_cached(reports=reports, code=code)
                self.assertEqual((kept, output), (False, ""))

    def test_ci_pins_reviewed_forgeds_digests(self):
        action = (REPO_ROOT / ".github/actions/setup-forgeds/action.yml").read_text()
        # The revision appears as the input default and as the digest table key.
        self.assertEqual(action.count(FORGEDS_REVISION), 2)
        self.assertIn(f"          {FORGEDS_REVISION})\n", action)
        for platform, digest in REVIEWED_ARCHIVE_DIGESTS.items():
            with self.subTest(platform=platform):
                self.assertIn(
                    f"              {platform})\n"
                    f"                archive_digest={digest}\n"
                    f"                binary_digest={REVIEWED_BINARY_DIGESTS[platform]}\n",
                    action,
                )
        # Only reviewed binaries are cached, under a key naming their digest.
        key = "-${{ inputs.revision }}-${{ steps.pins.outputs.binary_digest }}\n"
        restore = action.split("    - name: Restore forgeds binary\n", 1)[1].split("\n\n", 1)[0]
        self.assertIn("if: steps.pins.outputs.binary_digest != ''", restore)
        self.assertIn("uses: actions/cache/restore@v5", restore)
        self.assertTrue(restore.endswith(key.rstrip("\n")), restore)
        save = action.split("    - name: Save reviewed forgeds binary\n", 1)[1].split("\n\n", 1)[0]
        self.assertIn("if: steps.prebuilt.outputs.installed == 'true'", save)
        self.assertIn("uses: actions/cache/save@v5", save)
        self.assertTrue(save.endswith(key.rstrip("\n")), save)
        self.assertEqual(action.count("path: ~/.cargo/bin/forgeds"), 2)
        self.assertLess(
            action.index("- name: Verify forgeds revision"),
            action.index("- name: Save reviewed forgeds binary"),
        )
        # A restored binary is re-verified; every install step depends on that.
        self.assertNotIn("cache-hit != 'true'", action)
        self.assertEqual(action.count("steps.restored.outputs.verified != 'true'"), 6)
        self.assertIn("id: restored\n      if: steps.forgeds-cache.outputs.cache-hit == 'true'", action)

    def test_ci_provisions_the_exact_forgeds_source_revision(self):
        action = (
            REPO_ROOT / ".github/actions/setup-forgeds/action.yml"
        ).read_text()
        workflow = (REPO_ROOT / ".github/workflows/coverage.yml").read_text()
        self.assertIn(f"    default: {FORGEDS_REVISION}\n", action)
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
        # The prebuilt release binary is used only after digest and commit checks.
        self.assertIn("releases/download/forgeds\"", action)
        self.assertIn("forgeds-${FORGEDS_REVISION:0:7}_${PLATFORM}.tar.gz", action)
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

    def test_bytecode_check_compiles_with_pinned_forgeds(self):
        workflow = (REPO_ROOT / ".github/workflows/ci.yml").read_text()
        job = workflow.split("\n  bytecode-check:\n", 1)[1]
        job = job.split("\n\n  ", 1)[0]
        self.assertIn("uses: ./.github/actions/setup-forgeds", job)
        self.assertNotIn("foundry-rs/foundry-toolchain", job)
        self.assertLess(
            job.index("uses: ./.github/actions/setup-forgeds"),
            job.index("run: make bytecode-check"),
        )


    def test_foundry_artifact_caches_are_keyed_by_forgeds_and_solar(self):
        # Artifacts built by another Forge or Solar never satisfy a restore.
        prefix = (
            "foundry-build-${{ runner.os }}-forgeds-${{ steps.forgeds.outputs.revision }}"
            "-solar-${{ steps.solar.outputs.version }}-"
        )
        for name in ("ci.yml", "coverage.yml"):
            with self.subTest(workflow=name):
                workflow = (REPO_ROOT / ".github/workflows" / name).read_text()
                self.assertIn(
                    "- name: Setup forgeds\n        id: forgeds\n"
                    "        uses: ./.github/actions/setup-forgeds",
                    workflow,
                )
                solar = workflow.split("- name: Setup Solar 0.3.0 (Solidity 0.8.38)\n", 1)[1]
                solar = solar.split("\n      - name:", 1)[0]
                self.assertTrue(solar.startswith("        id: solar\n"))
                self.assertIn('test -n "$version"', solar)
                self.assertIn('echo "version=$version" >> "$GITHUB_OUTPUT"', solar)
                position = workflow.index("- name: Cache Foundry build artifacts")
                self.assertLess(workflow.index("id: forgeds"), position)
                self.assertLess(workflow.index("id: solar"), position)
                cache = workflow[position:].split("\n      - name:", 1)[0]
                keys = [
                    line.strip().removeprefix("key: ")
                    for line in cache.splitlines()
                    if "foundry-build-" in line
                ]
                self.assertEqual(len(keys), 4)
                for key in keys:
                    self.assertTrue(key.startswith(prefix), key)
                self.assertIn("hashFiles('scripts/install-solar.sh'", keys[0])


    def test_coverage_rust_cache_leaves_cargo_bin_to_the_forgeds_cache(self):
        # ~/.cargo/bin holds forgeds; only setup-forgeds may restore it, after
        # checking the reviewed digest.
        workflow = (REPO_ROOT / ".github/workflows/coverage.yml").read_text()
        steps = workflow.split("uses: Swatinem/rust-cache@v2\n")[1:]
        self.assertTrue(steps)
        for step in steps:
            self.assertIn("cache-bin: false", step.split("\n      - ", 1)[0])


if __name__ == "__main__":
    unittest.main()
