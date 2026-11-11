#!/usr/bin/env python3
"""Operate an explicitly configured, benchmark-owned Joltzu development network."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

MARKER = ".evm-benchmark-owner.json"


def setting(name):
    value = os.environ.get(name)
    if not value:
        raise ValueError(f"Set {name}; see this target's README.md")
    path = Path(value).expanduser().absolute()
    if path.resolve() != path:
        raise ValueError(f"{name} must have no symlink components")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["up", "down", "reset", "status"])
    command = parser.parse_args().command
    repo = setting("JOLTZU_REPO_DIR")
    data = setting("JOLTZU_DEVNET_DIR")
    tool = repo / "tools/devnet.sh"
    if not tool.is_file():
        raise ValueError(f"Missing Joltzu development launcher: {tool}")
    if data == repo or data in repo.parents or repo in data.parents:
        raise ValueError("Keep devnet data separate from the source directory")
    if data.parent == data or len(data.parts) < 4:
        raise ValueError("Choose a dedicated devnet data directory")
    identity = {
        "owner": "evm-benchmark-joltzu",
        "source": str(repo),
        "data": str(data),
        "validators": 4,
        "chain_id": int(os.environ.get("JOLTZU_CHAIN_ID", "31337")),
        "base_port": int(os.environ.get("JOLTZU_BASE_PORT", "30300")),
    }
    marker = data / MARKER
    if data.exists():
        if not marker.is_file() or marker.is_symlink():
            raise ValueError(f"Refusing devnet without benchmark ownership: {data}")
        if json.loads(marker.read_text()) != identity:
            raise ValueError(f"Devnet ownership/configuration does not match: {data}")
    elif command in {"down", "status"}:
        print("No benchmark-owned Joltzu devnet exists")
        return

    def run(action, *arguments):
        subprocess.run([str(tool), action, str(data), *arguments], cwd=repo, check=True)

    if command == "reset" and data.exists():
        run("clean")
    if command in {"up", "reset"}:
        if not data.exists():
            data.parent.mkdir(parents=True, exist_ok=True)
            run(
                "init",
                "--validators",
                "4",
                "--chain-id",
                str(identity["chain_id"]),
                "--base-port",
                str(identity["base_port"]),
            )
            with marker.open("x") as output:
                json.dump(identity, output, indent=2)
                output.write("\n")
        run("start")
    else:
        run("stop" if command == "down" else "status")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"joltzu: {error}", file=sys.stderr)
        sys.exit(1)
