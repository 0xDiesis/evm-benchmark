#!/usr/bin/env python3
"""Compare the Joltzu EVM smoke under two isolated cgroup v2 memory caps."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time
import uuid

MIB = 1024 * 1024
TARGETS = Path(__file__).resolve().parents[2]
CGROUP = Path("/sys/fs/cgroup")
OUTPUT = Path("/output")
PUBLIC_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
BINARIES = ("joltzu_validator", "joltzu_dev_signer", "joltzu_keygen", "joltzu_local")


def write_report(path, result):
    path.write_text(json.dumps(result, indent=2) + "\n")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(MIB), b""):
            digest.update(chunk)
    return digest.hexdigest()


def key_values(path):
    return {
        key: int(count)
        for key, count in (line.split() for line in path.read_text().splitlines())
    }


def sample_memory(cgroup):
    peak = cgroup / "memory.peak"
    pressure = cgroup / "memory.pressure"
    return {
        "current_bytes": int((cgroup / "memory.current").read_text()),
        "peak_bytes": int(peak.read_text()) if peak.exists() else None,
        "swap_bytes": int((cgroup / "memory.swap.current").read_text()),
        "events": key_values(cgroup / "memory.events"),
        "resident_anon_bytes": key_values(cgroup / "memory.stat")["anon"],
        "cpu_usec": key_values(cgroup / "cpu.stat")["usage_usec"],
        "pressure": pressure.read_text().splitlines() if pressure.exists() else None,
    }


def check_limits(cgroup, cap_mib):
    if (cgroup / "memory.max").read_text().strip() != str(cap_mib * MIB) or (
        cgroup / "memory.swap.max"
    ).read_text().strip() != "0":
        raise ValueError("Expected the exact private memory cap and zero swap")


def worker(cap_mib, openssl):
    check_limits(CGROUP, cap_mib)
    targets = Path("/bench-targets")
    env = dict(
        os.environ,
        PATH=f"/tools:{os.environ['PATH']}",
        JOLTZU_REPO_DIR="/node",
        JOLTZU_BUILD="/binaries",
        JOLTZU_DEVNET_DIR="/output/run/devnet",
        JOLTZU_BASE_PORT="30300",
        JOLTZU_CHAIN_ID="31337",
        JOLTZU_PROMETHEUS="0",
        OPENSSL=openssl,
        BENCH_KEY=PUBLIC_KEY,
        no_proxy="*",
        NO_PROXY="*",
    )
    launcher = [sys.executable, str(targets / "chains/joltzu/devnet.py")]
    smoke = [sys.executable, str(targets / "scripts/evm-smoke.py")]
    # Submit through validator one and check finality on all validators and the observer.
    for port in (30401, 30402, 30403, 30404, 30400):
        smoke.extend(("--rpc", f"http://127.0.0.1:{port}"))
    result = {"status": "failed", "before": sample_memory(CGROUP), "stages": {}}

    def run(stage, command):
        start = time.monotonic()
        try:
            subprocess.run(command, env=env, check=True)
        finally:
            result["stages"][stage] = {
                "elapsed_seconds": time.monotonic() - start,
                "memory": sample_memory(CGROUP),
            }

    before_restart = str(OUTPUT / "smoke.json")
    verified = False
    try:
        run("start", [*launcher, "up"])
        run("smoke", [*smoke, "--solc", "/tools/solc", "--out", before_restart])
        run("stop_before_restart", [*launcher, "down"])
        run("restart", [*launcher, "up"])
        run(
            "verify_restart",
            [*smoke, "--verify", before_restart, "--out", str(OUTPUT / "restart.json")],
        )
    except (OSError, subprocess.SubprocessError) as error:
        result["error"] = str(error)
    else:
        verified = True
    finally:
        try:
            run("stop", [*launcher, "down"])
            if verified:
                result["status"] = "passed"
        except (OSError, subprocess.SubprocessError) as error:
            result["stop_error"] = str(error)
        finally:
            write_report(OUTPUT / "profile.json", result)
    return int(result["status"] != "passed")


def assess(result, pressure):
    """Distinguish workload correctness from evidence of cgroup pressure."""
    before = result["before"]["events"]
    after = result["stages"]["stop"]["memory"]["events"]
    if any(after[key] > before[key] for key in ("oom", "oom_kill")):
        return "oom"
    if result["status"] != "passed":
        return "failed"
    if pressure:
        return "passed" if after["max"] > before["max"] else "cap-not-reached"
    return "baseline-reached-cap" if after["max"] > before["max"] else "passed"


def mount(path, target, readonly=True):
    if any(character in str(path) for character in (",", '"', "\n", "\r")):
        raise ValueError(
            f"Cannot bind Docker path containing commas, quotes or newlines: {path}"
        )
    return [
        "--mount",
        f"type=bind,src={path},dst={target}" + (",readonly" if readonly else ""),
    ]


def container_command(args, image, name, cap_mib, output):
    return [
        "docker",
        "run",
        "--rm",
        "--init",
        "--pull=never",
        "--network=none",
        "--cgroupns=private",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--read-only",
        "--pids-limit=512",
        f"--cpus={args.cpus}",
        f"--memory={cap_mib}m",
        f"--memory-swap={cap_mib}m",
        f"--name={name}",
        f"--user={os.getuid()}:{os.getgid()}",
        "--tmpfs",
        "/ramtmp:rw,nosuid,nodev,noexec,size=64m,mode=1777",
        "--env",
        "TMPDIR=/ramtmp",
        "--env",
        "HOME=/ramtmp",
        "--env",
        "PYTHONDONTWRITEBYTECODE=1",
        *mount(TARGETS, "/bench-targets"),
        *mount(args.node_repo / "tools/devnet.sh", "/node/tools/devnet.sh"),
        *mount(args.node_build, "/binaries"),
        *mount(args.cast, "/tools/cast"),
        *mount(args.solc, "/tools/solc"),
        *mount(output, "/output", readonly=False),
        "--entrypoint=timeout",
        image,
        "--kill-after=10s",
        f"{args.timeout}s",
        "python3",
        "/bench-targets/chains/joltzu/memory-check.py",
        "--worker",
        str(cap_mib),
        args.openssl,
    ]


def remove_container(name, log):
    try:
        outcome = subprocess.run(
            ["docker", "rm", "-f", name],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        detail = str(error)
    else:
        if outcome.returncode == 0:
            return True
        detail = outcome.stderr.strip()
        # Docker may have already removed the container because of --rm.
        if detail == f"Error response from daemon: No such container: {name}":
            return True
        detail = detail or f"Docker exited with code {outcome.returncode}"
    message = f"memory-check: Docker cleanup failed for {name} ({detail})"
    print(message, file=sys.stderr)
    print(f"\n{message}", file=log)
    return False


def run_profile(args, image, profile, cap_mib):
    output = args.out / profile
    output.mkdir()
    name = f"joltzu-memory-{uuid.uuid4().hex}"
    command = container_command(args, image, name, cap_mib, output)
    with (output / "console.log").open("x") as log:
        try:
            outcome = subprocess.run(
                command,
                stdout=log,
                stderr=subprocess.STDOUT,
                timeout=args.timeout + 15,
                check=False,
            )
        except subprocess.TimeoutExpired:
            return "timeout"
        finally:
            # A timed-out Docker client can leave its container running.
            removed = remove_container(name, log)
    result_path = output / "profile.json"
    if outcome.returncode == 124:
        return "timeout"
    if not result_path.is_file():
        return "failed"
    status = assess(json.loads(result_path.read_text()), profile == "pressure")
    return "failed" if (outcome.returncode or not removed) and status == "passed" else status


def main():
    if sys.argv[1:2] == ["--worker"]:
        return worker(int(sys.argv[2]), sys.argv[3])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, help="Local Linux toolchain image")
    parser.add_argument("--node-repo", type=Path, required=True)
    parser.add_argument("--node-build", type=Path, required=True)
    parser.add_argument("--solc", type=Path, required=True, help="Linux solc 0.8.37")
    parser.add_argument("--cast", type=Path, required=True, help="Linux Foundry cast")
    parser.add_argument(
        "--openssl",
        default="/opt/openssl/bin/openssl",
        help="OpenSSL 3 command path inside the image",
    )
    parser.add_argument("--baseline-mib", type=int, required=True)
    parser.add_argument("--pressure-mib", type=int, required=True)
    parser.add_argument("--cpus", type=int, default=2)
    parser.add_argument("--timeout", type=int, default=600, help="Seconds per profile")
    parser.add_argument(
        "--out", type=Path, required=True, help="New disposable output directory"
    )
    args = parser.parse_args()
    if not 64 <= args.pressure_mib < args.baseline_mib <= 4096:
        parser.error("Require 64 <= pressure MiB < baseline MiB <= 4096")
    if not 1 <= args.cpus <= 64 or not 1 <= args.timeout <= 900:
        parser.error("Require 1-64 CPUs and a 1-900 second timeout per profile")
    for setting in ("node_repo", "node_build", "solc", "cast", "out"):
        setattr(args, setting, getattr(args, setting).resolve())
    if any(
        args.out == source or source in args.out.parents
        for source in (args.node_repo, args.node_build, TARGETS.parent)
    ):
        parser.error("Keep output separate from source and binary directories")
    tools = {
        "solc": args.solc,
        "cast": args.cast,
        "devnet.sh": args.node_repo / "tools/devnet.sh",
        **{binary: args.node_build / binary for binary in BINARIES},
    }
    for tool in tools.values():
        if not tool.is_file() or not os.access(tool, os.X_OK):
            parser.error(f"Missing executable {tool}")
    image = subprocess.check_output(
        ["docker", "image", "inspect", "--format={{.Id}}", "--", args.image],
        text=True,
        timeout=15,
    ).strip()
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
        raise ValueError("Expected one local Docker image ID")
    args.out.mkdir(parents=True)
    summary = {
        "image_id": image,
        "cpus": args.cpus,
        "sha256": {
            name: sha256(path)
            for name, path in {
                **tools,
                "evm-smoke.py": TARGETS / "scripts/evm-smoke.py",
                "devnet.py": TARGETS / "chains/joltzu/devnet.py",
            }.items()
        },
        "profiles": {},
    }
    for profile, cap_mib in (
        ("baseline", args.baseline_mib),
        ("pressure", args.pressure_mib),
    ):
        status = run_profile(args, image, profile, cap_mib)
        summary["profiles"][profile] = {"cap_mib": cap_mib, "status": status}
        write_report(args.out / "summary.json", summary)
        print(f"{profile}: {status} ({cap_mib} MiB)")
        if status != "passed":
            return 1
    return 0


def terminate(_signum, _frame):
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"memory-check: {error}", file=sys.stderr)
        sys.exit(1)
