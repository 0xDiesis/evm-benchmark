#!/usr/bin/env python3
"""Run a four-validator Diesis fleet for benchmarks from a node image and a node checkout.

  fleet.py reset            fresh genesis (removes this tool's fresh fleet first)
  fleet.py up               start the fresh fleet, or the aged fleet when DIESIS_AGED_TAG is set
  fleet.py down             remove the fresh fleet; an aged fleet is only stopped
  fleet.py stop             stop whichever fleet runs, keeping its volumes
  fleet.py age --tag T      build an aged fleet: load it past --blocks, drain, stop
  fleet.py destroy-aged --tag T   remove an aged fleet and its manifest
  fleet.py status | render | provenance

Inputs (environment):
  DIESIS_REPO_DIR       node checkout whose docker/docker-compose.e2e.yml and
                        docker-compose.mpt-signing.yml are rendered (must match the image)
  DIESIS_IMAGE_TAG      diesis-node-e2e tag; default <checkout HEAD>-release. The image must
                        exist: this tool never builds (see build-image.sh)
  BENCH_DIESIS_PROFILE  capped (default; the official run rule) | uncapped (diagnosis);
                        profiles/<name>.env
  DIESIS_AGED_TAG       reuse the stopped aged fleet <tag> instead of a fresh genesis
  DIESIS_*, RAYON_*     validator environment overrides (win over the profile)
  E2E_<NAME>            node-Makefile spellings, applied as DIESIS_<NAME>
  BENCH_DIESIS_ENV      extra "K=V K=V" validator overrides

The compose is rendered with `docker compose config`, then patched: validators and
Prometheus only, the immutable image ID, host ports bound to 127.0.0.1, the profile's CPU,
cpuset and memory limits, and the merged environment on every validator. Container names
stay diesis-e2e-node-N so network-topology.sh works; the tool refuses to touch containers
or ports that belong to another project (for example a `make e2e-up` cluster).

Aged fleets: validators bind their writer-fence journal to the datadir's device and inode,
so volumes cannot be copied. An aged fleet is a stopped compose project
(diesis-bench-aged-<tag>) whose volumes every later run reuses; each run re-renders it for
its own image, profile and overrides. The chain keeps aging from run to run.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import sys
import time
from urllib.request import Request, urlopen

HERE = Path(__file__).resolve().parent
BENCH_ROOT = HERE.parents[2]
NODES = ["node-1", "node-2", "node-3", "node-4"]
RPC_PORTS = [18545, 18555, 18565, 18575]
METRICS_PORTS = [19101, 19102, 19103, 19104]
FRESH_PROJECT = "diesis-bench"
OWNER_LABEL = "local.bench.fleet"
DEV_KEY = "0x" + "0" * 63 + "1"
RESERVED = {"DIESIS_REPO_DIR", "DIESIS_RPC", "DIESIS_WS", "DIESIS_CHAIN_ID", "DIESIS_IMAGE_TAG", "DIESIS_AGED_TAG",
            "DIESIS_IMAGE_PROFILE", "DIESIS_SOURCE_REVISION"}


def state_dir():
    base = Path(os.environ.get("BENCH_RESULTS_BASE", BENCH_ROOT / "bench-targets" / "results"))
    path = base / "fleet"
    path.mkdir(parents=True, exist_ok=True)
    return path


def aged_project(tag):
    if not re.fullmatch(r"[a-z0-9-]{1,24}", tag):
        raise SystemExit("Aged tag must match [a-z0-9-]{1,24}")
    return f"diesis-bench-aged-{tag}"


def say(text):
    print(f"[fleet] {text}", flush=True)


def sh(args, check=True, capture=False, env=None, timeout=600):
    return subprocess.run(list(map(str, args)), check=check, text=True, timeout=timeout, env=env,
                          capture_output=capture)


def rpc(port, method, params=(), timeout=5):
    body = json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=list(params))).encode()
    with urlopen(Request(f"http://127.0.0.1:{port}", body, {"Content-Type": "application/json"}),
                 timeout=timeout) as response:
        data = json.load(response)
    if "error" in data:
        raise RuntimeError(f"{method}: {data['error']}")
    return data["result"]


def heights():
    out = []
    for port in RPC_PORTS:
        try:
            out.append(int(rpc(port, "eth_blockNumber"), 16))
        except Exception:
            out.append(None)
    return out


# ---- inputs --------------------------------------------------------------------------

def checkout():
    path = Path(os.environ.get("DIESIS_REPO_DIR", BENCH_ROOT.parent / "diesis")).resolve()
    if not (path / "docker" / "docker-compose.e2e.yml").is_file():
        raise SystemExit(f"No node checkout with docker/docker-compose.e2e.yml at {path}; set DIESIS_REPO_DIR")
    return path


def image_ref(repo):
    tag = os.environ.get("DIESIS_IMAGE_TAG")
    if not tag:
        head = sh(["git", "-C", repo, "rev-parse", "HEAD"], capture=True).stdout.strip()
        tag = f"{head}-release"
    ref = f"diesis-node-e2e:{tag}"
    found = sh(["docker", "image", "inspect", ref, "--format", "{{.Id}}"], check=False, capture=True)
    if found.returncode:
        raise SystemExit(f"Image {ref} not found. Build it with bench-targets/chains/diesis/build-image.sh "
                         f"(through the compute broker), or set DIESIS_IMAGE_TAG.")
    return ref, found.stdout.strip()


def read_profile(name):
    path = HERE / "profiles" / f"{name}.env"
    if not path.is_file():
        raise SystemExit(f"Unknown profile {name}: no {path}")
    values = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise SystemExit(f"Bad profile line {line!r} in {path}")
        values[key] = value
    return values


def merged_env(profile_values, environ):
    """Validator environment: profile, then E2E_* spellings, then DIESIS_*/RAYON_*, then BENCH_DIESIS_ENV."""
    env = {k: v for k, v in profile_values.items() if not k.startswith("FLEET_")}
    for key, value in environ.items():
        if key.startswith("E2E_") and value != "":
            env["DIESIS_" + key[4:]] = value
    for key, value in environ.items():
        if re.fullmatch(r"(DIESIS|RAYON)_[A-Z0-9_]+", key) and key not in RESERVED and value != "":
            env[key] = value
    for item in shlex.split(environ.get("BENCH_DIESIS_ENV", "")):
        key, sep, value = item.partition("=")
        if not sep or not re.fullmatch(r"(DIESIS|RAYON)_[A-Z0-9_]+", key):
            raise SystemExit(f"Bad BENCH_DIESIS_ENV entry {item!r}")
        env[key] = value
    return env


def render(repo, image_id, tag, profile_name, environ=None):
    environ = dict(os.environ if environ is None else environ)
    profile = read_profile(profile_name)
    env = merged_env(profile, environ)
    files = ["-f", repo / "docker" / "docker-compose.e2e.yml"]
    signing = repo / "docker" / "docker-compose.mpt-signing.yml"
    if signing.is_file():
        files += ["-f", signing]
    head = sh(["git", "-C", repo, "rev-parse", "HEAD"], capture=True).stdout.strip()
    render_env = dict(environ, COMPOSE_DISABLE_ENV_FILE="true", COMPOSE_PROFILES="monitoring",
                      DIESIS_IMAGE_TAG=tag, DIESIS_SOURCE_REVISION=head, DIESIS_IMAGE_PROFILE="release", **env)
    rendered = json.loads(sh(["docker", "compose", *files, "config", "--format", "json"], capture=True,
                             env=render_env).stdout)
    interpolated = set()
    for path in files[1::2]:
        interpolated |= set(re.findall(r"\$\{([A-Z0-9_]+)", Path(path).read_text()))
    compose = patch(rendered, image_id, env, profile, interpolated)
    return compose, dict(checkout=str(repo), checkout_head=head, profile=profile_name, profile_values=profile,
                         validator_env=env, image_id=image_id, signing_overlay=signing.is_file())


def patch(rendered, image_id, env, profile, interpolated=frozenset()):
    """Validators take every override except keys the compose only interpolates (log levels,
    txpool flags, Rayon thread count): the node's strict configuration rejects any DIESIS_*
    key it does not consume, so those reach the node only through the rendered values."""
    keep = NODES + ["prometheus"]
    missing = [name for name in keep if name not in rendered["services"]]
    if missing:
        raise SystemExit(f"Rendered compose lacks {missing}")
    services = {name: rendered["services"][name] for name in keep}
    env_keys = {k for name in NODES for k in services[name].get("environment", {})}
    container_env = {k: v for k, v in env.items() if k in env_keys or k not in interpolated}
    cpus = profile.get("FLEET_CPUS")
    memory = profile.get("FLEET_MEMORY")
    pin = profile.get("FLEET_PIN_CPUS") == "true"
    for i, name in enumerate(NODES):
        service = services[name]
        service.pop("build", None)
        service.pop("depends_on", None)
        service["image"] = image_id
        service["environment"].update(container_env)
        for key in ("cpus", "cpuset", "mem_limit", "deploy"):
            service.pop(key, None)
        if cpus:
            service["cpus"] = float(cpus)
            if pin:
                n = int(float(cpus))
                service["cpuset"] = f"{i * n}-{i * n + n - 1}" if n > 1 else str(i)
        if memory:
            service["mem_limit"] = memory
    for service in services.values():
        service.setdefault("labels", {})
        if isinstance(service["labels"], list):
            service["labels"] = dict(item.partition("=")[::2] for item in service["labels"])
        service["labels"][OWNER_LABEL] = "true"
        for port in service.get("ports", []):
            port["host_ip"] = "127.0.0.1"
    used = {v["source"] for s in services.values() for v in s.get("volumes", []) if v.get("type") == "volume"}
    return dict(services=services, networks=rendered["networks"],
                volumes={k: v for k, v in rendered.get("volumes", {}).items() if k in used})


# ---- ownership -----------------------------------------------------------------------

def containers(filters):
    out = sh(["docker", "ps", "-a", "--format", "{{.Names}}\t{{.Label \"com.docker.compose.project\"}}\t{{.State}}",
              *sum((["--filter", f] for f in filters), [])], capture=True).stdout
    return [tuple(line.split("\t")) for line in out.splitlines() if line]


def foreign_fleet(project):
    """Containers holding the validator names (or this tool's fleets) outside `project`."""
    names = containers(["name=^diesis-e2e-node-[1-4]$"])
    return [(n, p, s) for n, p, s in names if p != project]


def running_project():
    names = containers(["name=^diesis-e2e-node-[1-4]$", "status=running"])
    projects = {p for _, p, _ in names}
    return projects.pop() if len(projects) == 1 else None


def port_holders():
    busy = []
    for port in RPC_PORTS + METRICS_PORTS:
        out = sh(["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN"], check=False, capture=True).stdout
        if out.strip():
            busy.append(port)
    return busy


def compose_cmd(project, path):
    return ["docker", "compose", "--project-name", project, "-f", path]


def compose_env():
    return dict(os.environ, COMPOSE_DISABLE_ENV_FILE="true")


def wait_live(timeout=180):
    deadline = time.monotonic() + timeout
    first = None
    while time.monotonic() < deadline:
        h = heights()
        if first is None and all(x is not None for x in h):
            first = h
        if first and all(x is not None and x > b for x, b in zip(h, first)):
            say(f"live heights {h}")
            return h
        time.sleep(1)
    raise SystemExit(f"Validators did not all advance within {timeout} s (heights {heights()})")


# ---- commands ------------------------------------------------------------------------

def start(project, aged_tag=None):
    others = foreign_fleet(project)
    if others:
        running = [n for n, p, s in others if s == "running"]
        if running or any(not p.startswith(FRESH_PROJECT) for _, p, _ in others):
            raise SystemExit(f"Validator containers belong to another project: {others}. Stop that cluster first "
                             f"(`make -C <node> e2e-down`, or `fleet.py stop`).")
        # A stopped fleet of this tool under another project: remove its containers, keep volumes.
        for name, p, _ in others:
            sh(["docker", "rm", name], check=False, capture=True)
    if running_project() != project and port_holders():
        raise SystemExit(f"Benchmark ports in use: {port_holders()}")
    repo = checkout()
    ref, image_id = image_ref(repo)
    profile = os.environ.get("BENCH_DIESIS_PROFILE", "capped")
    compose, provenance = render(repo, image_id, ref.split(":", 1)[1], profile)
    directory = state_dir() / project
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "compose.json"
    path.write_text(json.dumps(compose, indent=2) + "\n")
    provenance.update(project=project, image=ref, aged_tag=aged_tag,
                      compose_sha256=hashlib.sha256(path.read_bytes()).hexdigest(), started=time.time())
    (directory / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    say(f"{project}: {ref} ({image_id[:19]}) profile={profile} checkout={repo}")
    sh([*compose_cmd(project, path), "up", "-d", "--no-build", "--pull", "never", "--remove-orphans"],
       env=compose_env(), timeout=600)
    return wait_live()


def cmd_reset(_a):
    path = last_compose(FRESH_PROJECT)
    if path:
        sh([*compose_cmd(FRESH_PROJECT, path), "down", "--volumes", "--remove-orphans"], check=False,
           env=compose_env())
    start(FRESH_PROJECT)


def last_compose(project):
    path = state_dir() / project / "compose.json"
    return path if path.exists() else None


def cmd_up(_a):
    tag = os.environ.get("DIESIS_AGED_TAG")
    if tag:
        manifest(tag)
        start(aged_project(tag), tag)
    else:
        start(FRESH_PROJECT)


def stop_project(project):
    path = last_compose(project)
    if path:
        sh([*compose_cmd(project, path), "stop", "-t", "120"], check=False, env=compose_env())


def cmd_down(_a):
    tag = os.environ.get("DIESIS_AGED_TAG")
    if tag:
        stop_project(aged_project(tag))
        return
    path = last_compose(FRESH_PROJECT)
    if path:
        sh([*compose_cmd(FRESH_PROJECT, path), "down", "--volumes", "--remove-orphans"], check=False,
           env=compose_env())


def cmd_stop(_a):
    project = running_project()
    if project and (project == FRESH_PROJECT or project.startswith("diesis-bench-aged-")):
        stop_project(project)
    elif project:
        raise SystemExit(f"Running validators belong to {project}, not this tool")


def manifest_path(tag):
    return state_dir() / f"aged-{tag}.json"


def manifest(tag):
    path = manifest_path(tag)
    if not path.exists():
        raise SystemExit(f"No aged fleet {tag} ({path}); build one with `fleet.py age --tag {tag}`")
    return json.loads(path.read_text())


def harness_binary():
    explicit = os.environ.get("BENCH_HARNESS")
    if explicit:
        return Path(explicit)
    meta = json.loads(sh(["cargo", "metadata", "--no-deps", "--format-version", "1", "--manifest-path",
                          BENCH_ROOT / "Cargo.toml"], capture=True).stdout)
    binary = Path(meta["target_directory"]) / "release" / "evm-benchmark"
    if not binary.exists():
        raise SystemExit(f"Build the harness first (make build): {binary} missing")
    return binary


def pool_pending():
    total = []
    for port in METRICS_PORTS:
        with urlopen(f"http://127.0.0.1:{port}/metrics", timeout=10) as response:
            for line in response.read().decode().splitlines():
                if line.startswith("reth_transaction_pool_pending_pool_transactions "):
                    total.append(float(line.split()[-1]))
    return total


def cmd_age(a):
    project = aged_project(a.tag)
    if manifest_path(a.tag).exists():
        raise SystemExit(f"Aged fleet {a.tag} exists")
    os.environ.setdefault("BENCH_DIESIS_PROFILE", "uncapped")
    start(project, a.tag)
    harness = harness_binary()
    urls = ",".join(f"http://127.0.0.1:{p}" for p in RPC_PORTS)
    env = dict(os.environ, BENCH_KEY=DEV_KEY, BENCH_ASSUME_ISOLATED_BLOCKS="false", BENCH_CONFIRM_WAIT_SECS="5",
               BENCH_PRESIGN_HEADROOM="1.1", BENCH_FUND_CONFIRM_TIMEOUT_SECS="240",
               BENCH_FUND_CHAIN_READY_TIMEOUT_SECS="180", BENCH_FUND_DEPLOY_TIMEOUT_SECS="180")
    out = state_dir() / project
    common = [harness, "--rpc-endpoints", urls, "--validators", urls, "--ws", "ws://127.0.0.1:18546",
              "--chain-id", "19803", "--senders", str(a.senders), "--workers", "64", "--batch-size", "200",
              "--retry-profile", "off", "--finality-confirmations", "0", "--test", "transfer",
              "--bench-name", project, "--quiet"]
    for attempt in range(1, 5):
        target = out / f"fund-{attempt}.json"
        result = sh([*common, "--fund", "--execution", "burst", "--txs", "4", "--waves", "1", "--out", target],
                    check=False, env=env, timeout=900)
        if result.returncode == 0 and json.loads(target.read_text())["results"]["confirmed"] == 4:
            break
        time.sleep(30)
    else:
        raise SystemExit("Sender funding failed four times")
    gen = subprocess.Popen(list(map(str, [*common, "--execution", "sustained", "--duration", max(120, a.blocks * 2),
                                          "--tps", a.rate, "--out", out / "aging.json"])), env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + a.max_minutes * 60
        while True:
            time.sleep(15)
            h = heights()
            say(f"aging heights {h}")
            if all(x is not None for x in h) and min(h) >= a.blocks:
                break
            if gen.poll() is not None or time.monotonic() > deadline:
                raise SystemExit(f"Aging stopped short at {h}")
    finally:
        if gen.poll() is None:
            gen.send_signal(signal.SIGINT)
            try:
                gen.wait(timeout=60)
            except subprocess.TimeoutExpired:
                gen.kill()
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline and max(pool_pending() or [0]) > 0:
        time.sleep(5)
    time.sleep(10)
    record = dict(tag=a.tag, project=project, height=min(heights()), aging_rate=a.rate, senders=a.senders,
                  created=time.time(), provenance=json.loads((out / "provenance.json").read_text()))
    stop_project(project)
    manifest_path(a.tag).write_text(json.dumps(record, indent=2) + "\n")
    say(f"aged fleet {a.tag} at height {record['height']}, stopped")


def cmd_destroy_aged(a):
    project = aged_project(a.tag)
    path = last_compose(project)
    if path:
        sh([*compose_cmd(project, path), "down", "--volumes", "--remove-orphans"], env=compose_env())
    manifest_path(a.tag).unlink(missing_ok=True)


def cmd_provenance(_a):
    """Print the running fleet's provenance (bench.sh stores it beside each report)."""
    project = running_project()
    path = state_dir() / project / "provenance.json" if project else None
    print(path.read_text() if path and path.exists() else "{}")


def cmd_status(_a):
    print(json.dumps(dict(running_project=running_project(), heights=heights()), indent=2))


def cmd_render(_a):
    repo = checkout()
    ref, image_id = image_ref(repo)
    compose, provenance = render(repo, image_id, ref.split(":", 1)[1], os.environ.get("BENCH_DIESIS_PROFILE", "capped"))
    print(json.dumps(dict(provenance=provenance, compose=compose), indent=2))


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name in ("reset", "up", "down", "stop", "status", "render", "provenance"):
        sub.add_parser(name)
    age = sub.add_parser("age")
    age.add_argument("--tag", required=True)
    age.add_argument("--blocks", type=int, default=5100)
    age.add_argument("--rate", type=int, default=400)
    age.add_argument("--senders", type=int, default=512)
    age.add_argument("--max-minutes", type=int, default=120)
    destroy = sub.add_parser("destroy-aged")
    destroy.add_argument("--tag", required=True)
    a = p.parse_args()
    globals()["cmd_" + a.cmd.replace("-", "_")](a)


if __name__ == "__main__":
    main()
