#!/usr/bin/env python3
"""Capture tuning metrics from the local Joltzu benchmark devnet."""

import argparse
from http.client import HTTPConnection, HTTPException
import json
import math
import os
from pathlib import Path
import re
import sys
import time

REQUIRED = {
    "joltzu_finalized_height",
    "joltzu_mailbox_dropped_frames_total",
    "joltzu_peer_send_timeouts_total",
}
OPTIONAL_COUNTS = {
    "joltzu_signer_requests_total",
    "joltzu_signer_failures_total",
    "joltzu_durable_commit_attempts_total",
    "joltzu_durable_commit_failures_total",
}
DURATIONS = {"joltzu_signer_seconds_total", "joltzu_durable_commit_seconds_total"}
SAMPLED_NAMES = REQUIRED | OPTIONAL_COUNTS | DURATIONS
MAX_RESPONSE_BYTES = 64 * 1024


def parse_samples(text):
    """Read the node's fixed, unlabelled samples, preserving integer counters."""
    samples = {}
    for line in text.splitlines():
        fields = line.split()
        if not fields:
            continue
        name = fields[0]
        if name not in SAMPLED_NAMES:
            continue
        if len(fields) != 2 or name in samples:
            raise ValueError(f"invalid or duplicate sample for {name}")
        raw = fields[1]
        if name in DURATIONS:
            value = float(raw)
            if not math.isfinite(value) or value < 0:
                raise ValueError(f"invalid sample value for {name}")
        else:
            if not re.fullmatch(r"[0-9]+", raw):
                raise ValueError(f"invalid sample value for {name}")
            value = int(raw)
        samples[name] = value
    missing = REQUIRED - samples.keys()
    if missing:
        raise ValueError("missing samples: " + ", ".join(sorted(missing)))
    return samples


def scrape(port):
    connection = HTTPConnection("127.0.0.1", port, timeout=2)
    try:
        connection.request("GET", "/metrics", headers={"Connection": "close"})
        with connection.getresponse() as response:
            if response.status != 200:
                raise ValueError(f"HTTP {response.status}")
            content = response.read(MAX_RESPONSE_BYTES + 1)
            if len(content) > MAX_RESPONSE_BYTES:
                raise ValueError("metrics response exceeds size limit")
            return parse_samples(content.decode("utf-8"))
    finally:
        connection.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-port", type=int, default=os.environ.get("JOLTZU_BASE_PORT", "30300"))
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args(argv)
    if not 1024 <= args.base_port <= 65535 - 104:
        parser.error("--base-port must leave room for five RPC listeners")

    started_at = time.time()
    nodes = {}
    failed = False
    for index, name in enumerate(("observer", "node1", "node2", "node3", "node4")):
        port = args.base_port + 100 + index
        endpoint = f"http://127.0.0.1:{port}/metrics"
        try:
            nodes[name] = {"url": endpoint, "samples": scrape(port)}
        except (OSError, ValueError, HTTPException) as error:
            nodes[name] = {"url": endpoint, "error": str(error)}
            print(f"joltzu metrics ({name}): {error}", file=sys.stderr)
            failed = True
    snapshot = {"started_at_unix": started_at, "finished_at_unix": time.time(), "nodes": nodes}
    args.out.write_text(json.dumps(snapshot, indent=2) + "\n", encoding="utf-8")
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
