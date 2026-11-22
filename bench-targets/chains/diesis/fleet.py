#!/usr/bin/env python3
"""Run a four-validator Diesis fleet for benchmarks from a node image and a node checkout.

  fleet.py reset            fresh genesis (removes this user's fresh fleet first)
  fleet.py up               start the fresh fleet, or the aged fleet when DIESIS_AGED_TAG is set
  fleet.py down             remove the fresh fleet; an aged fleet is only stopped
  fleet.py stop             stop the selected fleet, keeping its volumes
  fleet.py age --tag T      build an aged fleet: load it past --blocks, drain, stop
  fleet.py destroy-aged --tag T   remove an aged fleet and its manifest
  fleet.py save-fixture --tag T   archive a stopped aged fleet's volumes (fixture, never modified)
  fleet.py restore-fixture --fixture NAME --as T   extract a fixture into a new aged fleet T
  fleet.py status | render | provenance | ports

Inputs (environment):
  DIESIS_REPO_DIR       node checkout whose docker/docker-compose.e2e.yml and
                        docker-compose.mpt-signing.yml are rendered (must match the image)
  DIESIS_IMAGE_TAG      diesis-node-e2e tag; default <checkout HEAD>-release. The image must
                        exist: this tool never builds (see build-image.sh)
  BENCH_DIESIS_PROFILE  capped (default; the official run rule) | uncapped (diagnosis);
                        profiles/<name>.env
  DIESIS_AGED_TAG       reuse the stopped aged fleet <tag> instead of a fresh genesis
  BENCH_DIESIS_FLEET    this user's fresh fleet name (default diesis-bench); concurrent users
                        on one host pick distinct names
  BENCH_DIESIS_OUTCOME_RULE  genesis outcome signature rule: bls-only (default) or hybrid. The
                        node's live validator path does not yet sign the ML-DSA halves hybrid
                        outcomes need, so a hybrid fleet stops within a few blocks. Checkouts
                        without docker/epoch/epoch-zero-4v-bls-only.calldata predate the
                        control and get no rule keys (their strict configuration refuses them)
  BENCH_DIESIS_MAX_FLEETS  start only while fewer than N other Diesis fleets (any user's, one per
                        compose project) run on this host; otherwise wait, polling every 30 s,
                        up to BENCH_DIESIS_SLOT_WAIT_S seconds (default 7200)
  BENCH_DIESIS_HOLD_FILE   while this file exists, fleet starts wait too: creating it yields the
                        slot at a runner's next arm, which tears its fleet down and then waits
  BENCH_DIESIS_ENV      "K=V K=V" validator overrides (win over the profile), for any node flag
  E2E_<NAME>            node-Makefile spellings, applied as DIESIS_<NAME> overrides
  DIESIS_*              ambient values only feed the compose's own ${...} interpolation; they
                        are never injected (workspace tooling exports DIESIS_* names the
                        node's strict configuration would reject)

The compose is rendered with `docker compose config`, then patched: validators and
Prometheus only, the immutable image ID, host ports bound to 127.0.0.1, the profile's CPU,
cpuset and memory limits, and the merged environment on every validator.

Port blocks: every fleet gets the first free block (block k adds 200*k to the published
RPC 18545-18575, WS 18546, metrics 19101-19104 and Prometheus 19090 ports and owns subnet
10.100.(100+k).0/24); a port counts as busy when anything listens on it or Docker publishes
it, a subnet when any Docker network uses it. A project keeps its previous block while that
block stays free. P2P ports stay inside each fleet's network. The block, container names
(<project>-node-N) and node IPs go to <state>/<project>/ports.json and the provenance, which
bench.sh stores with every run; lib.sh routes RPC, WS, metrics and topology from it.

CPU pinning: a profile with FLEET_PIN_CPUS=true pins validator i to FLEET_CPUS consecutive CPUs
from a base. A lone pinned fleet uses base 0 (CPUs 0-7 under the capped profile), so results
stay comparable with earlier runs. A fleet that starts while other Diesis fleets pin CPUs takes
whichever of base 0 and <Docker CPUs> - width overlaps those CPUs least: CPUs 4-11 beside a
capped fleet on a 12-CPU Docker VM. The base and every validator's cpuset go to the provenance.

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
# Port blocks: block k adds 200*k to every published port and owns subnet 10.100.(100+k).
# P2P (30303, 30334) stays inside each fleet's own network and is never published.
BLOCK_STRIDE = 200
MAX_BLOCKS = 20
DEFAULT_FLEET = "diesis-bench"
OWNER_LABEL = "local.bench.fleet"
DEV_KEY = "0x" + "0" * 63 + "1"
RESERVED = {"DIESIS_REPO_DIR", "DIESIS_RPC", "DIESIS_WS", "DIESIS_CHAIN_ID", "DIESIS_IMAGE_TAG", "DIESIS_AGED_TAG",
            "DIESIS_IMAGE_PROFILE", "DIESIS_SOURCE_REVISION"}
# Genesis outcome signature rule. Each rule has its own genesis hash, so the epoch-zero
# calldata the validators load must be the artifact signed for that rule's genesis.
OUTCOME_RULES = ("bls-only", "hybrid")
DEFAULT_OUTCOME_RULE = "bls-only"
BLS_ONLY_EPOCH_ZERO = "epoch-zero-4v-bls-only.calldata"
OUTCOME_RULE_KEYS = ("DIESIS_DEV_OUTCOME_SIGNATURE_RULE", "DIESIS_EPOCH_ZERO_CALLDATA_FILE")


def state_dir():
    base = Path(os.environ.get("BENCH_RESULTS_BASE", BENCH_ROOT / "bench-targets" / "results"))
    path = base / "fleet"
    path.mkdir(parents=True, exist_ok=True)
    return path


def fresh_project():
    """This user's disposable fleet; concurrent users pick distinct BENCH_DIESIS_FLEET names."""
    name = os.environ.get("BENCH_DIESIS_FLEET", DEFAULT_FLEET)
    if not re.fullmatch(r"[a-z][a-z0-9-]{0,30}", name):
        raise SystemExit("BENCH_DIESIS_FLEET must match [a-z][a-z0-9-]{0,30}")
    return name


def aged_project(tag):
    if not re.fullmatch(r"[a-z0-9-]{1,24}", tag):
        raise SystemExit("Aged tag must match [a-z0-9-]{1,24}")
    return f"diesis-bench-aged-{tag}"


def selected_project():
    tag = os.environ.get("DIESIS_AGED_TAG")
    return aged_project(tag) if tag else fresh_project()


def block_ports(k):
    o = BLOCK_STRIDE * k
    return dict(block=k, rpc=[18545 + o, 18555 + o, 18565 + o, 18575 + o], ws=18546 + o,
                metrics=[19101 + o, 19102 + o, 19103 + o, 19104 + o], prometheus=19090 + o,
                subnet_prefix=f"10.100.{100 + k}")


def host_ports(block):
    return block["rpc"] + [block["ws"]] + block["metrics"] + [block["prometheus"]]


def busy_host_ports():
    """Every TCP port listening on this host, plus every Docker-published host port."""
    busy = set()
    out = sh(["lsof", "-nP", "-iTCP", "-sTCP:LISTEN"], check=False, capture=True).stdout
    busy |= {int(m) for m in re.findall(r":(\d+) \(LISTEN\)", out)}
    out = sh(["docker", "ps", "--format", "{{.Ports}}"], check=False, capture=True).stdout
    busy |= {int(m) for m in re.findall(r":(\d+)->", out)}
    return busy


def busy_subnets(own_network=None):
    names = sh(["docker", "network", "ls", "--format", "{{.Name}}"], check=False, capture=True).stdout.split()
    names = [n for n in names if n != own_network]
    if not names:
        return set()
    out = sh(["docker", "network", "inspect", "--format", "{{range .IPAM.Config}}{{.Subnet}} {{end}}", *names],
             check=False, capture=True).stdout
    return {".".join(net.split("/")[0].split(".")[:3]) for net in out.split()}


def choose_block(previous, ports_busy, subnets_busy):
    """The project's previous block if still free, else the first free one."""
    def free(block):
        return not (set(host_ports(block)) & ports_busy) and block["subnet_prefix"] not in subnets_busy
    if previous is not None and free(block_ports(previous)):
        return block_ports(previous)
    for k in range(MAX_BLOCKS):
        if free(block_ports(k)):
            return block_ports(k)
    raise SystemExit(f"No free port block among {MAX_BLOCKS}")


def ports_path(project):
    return state_dir() / project / "ports.json"


def project_ports(project):
    path = ports_path(project)
    return json.loads(path.read_text()) if path.exists() else block_ports(0)


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


def heights(block):
    out = []
    for port in block["rpc"]:
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
    """Explicit validator overrides: profile, then E2E_* spellings, then BENCH_DIESIS_ENV."""
    env = {k: v for k, v in profile_values.items() if not k.startswith("FLEET_")}
    for key, value in environ.items():
        if key.startswith("E2E_") and value != "":
            env["DIESIS_" + key[4:]] = value
    for item in shlex.split(environ.get("BENCH_DIESIS_ENV", "")):
        key, sep, value = item.partition("=")
        if not sep or not re.fullmatch(r"(DIESIS|RAYON)_[A-Z0-9_]+", key):
            raise SystemExit(f"Bad BENCH_DIESIS_ENV entry {item!r}")
        env[key] = value
    return env


def outcome_rule_env(repo, environ, default=DEFAULT_OUTCOME_RULE):
    """Validator keys selecting the genesis outcome signature rule, and the rule (None when the
    checkout predates the control). The rule and its epoch-zero artifact always travel together."""
    requested = environ.get("BENCH_DIESIS_OUTCOME_RULE") or None
    rule = requested or default
    if rule not in OUTCOME_RULES:
        raise SystemExit(f"BENCH_DIESIS_OUTCOME_RULE must be one of {', '.join(OUTCOME_RULES)}, not {rule!r}")
    if not (Path(repo) / "docker" / "epoch" / BLS_ONLY_EPOCH_ZERO).is_file():
        if requested:
            raise SystemExit(f"{repo} predates the genesis outcome rule control; unset BENCH_DIESIS_OUTCOME_RULE")
        return {}, None
    env = {"DIESIS_DEV_OUTCOME_SIGNATURE_RULE": rule}
    if rule == "bls-only":
        env["DIESIS_EPOCH_ZERO_CALLDATA_FILE"] = f"/etc/diesis/epoch/{BLS_ONLY_EPOCH_ZERO}"
    return env, rule


def render(repo, image_id, tag, profile_name, environ=None, project=DEFAULT_FLEET, block=None,
           outcome_rule=DEFAULT_OUTCOME_RULE, cpu_base=0):
    environ = dict(os.environ if environ is None else environ)
    profile = read_profile(profile_name)
    env = merged_env(profile, environ)
    clash = sorted(set(OUTCOME_RULE_KEYS) & set(env))
    if clash:
        raise SystemExit(f"Select the outcome rule with BENCH_DIESIS_OUTCOME_RULE, not {', '.join(clash)}")
    rule_env, rule = outcome_rule_env(repo, environ, outcome_rule)
    env.update(rule_env)
    files = ["-f", repo / "docker" / "docker-compose.e2e.yml"]
    signing = repo / "docker" / "docker-compose.mpt-signing.yml"
    if signing.is_file():
        files += ["-f", signing]
    head = sh(["git", "-C", repo, "rev-parse", "HEAD"], capture=True).stdout.strip()
    block = block or block_ports(0)
    ports_env = {f"DIESIS_E2E_NODE{i + 1}_RPC_PORT": str(p) for i, p in enumerate(block["rpc"])}
    ports_env |= {f"DIESIS_E2E_NODE{i + 1}_METRICS_PORT": str(p) for i, p in enumerate(block["metrics"])}
    ports_env |= dict(DIESIS_E2E_NODE1_WS_PORT=str(block["ws"]), DIESIS_E2E_PROMETHEUS_PORT=str(block["prometheus"]),
                      DIESIS_E2E_SUBNET_PREFIX=block["subnet_prefix"])
    render_env = dict(environ, COMPOSE_DISABLE_ENV_FILE="true", COMPOSE_PROFILES="monitoring",
                      DIESIS_IMAGE_TAG=tag, DIESIS_SOURCE_REVISION=head, DIESIS_IMAGE_PROFILE="release", **env,
                      **ports_env)
    rendered = json.loads(sh(["docker", "compose", *files, "config", "--format", "json"], capture=True,
                             env=render_env).stdout)
    interpolated = set()
    for path in files[1::2]:
        interpolated |= set(re.findall(r"\$\{([A-Z0-9_]+)", Path(path).read_text()))
    compose = patch(rendered, image_id, env, profile, interpolated, project, cpu_base)
    return compose, dict(checkout=str(repo), checkout_head=head, profile=profile_name, profile_values=profile,
                         validator_env=env, image_id=image_id, signing_overlay=signing.is_file(),
                         outcome_signature_rule=rule, cpu_base=cpu_base,
                         cpusets={name: compose["services"][name].get("cpuset") for name in NODES})


def patch(rendered, image_id, env, profile, interpolated=frozenset(), project=DEFAULT_FLEET, cpu_base=0):
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
                first = cpu_base + i * n
                service["cpuset"] = f"{first}-{first + n - 1}" if n > 1 else str(first)
        if memory:
            service["mem_limit"] = memory
    for name, service in services.items():
        # Per-project names, so fleets of different projects never collide.
        service["container_name"] = f"{project}-{name}"
        service.setdefault("labels", {})
        if isinstance(service["labels"], list):
            service["labels"] = dict(item.partition("=")[::2] for item in service["labels"])
        service["labels"][OWNER_LABEL] = "true"
        for port in service.get("ports", []):
            port["host_ip"] = "127.0.0.1"
    used = {v["source"] for s in services.values() for v in s.get("volumes", []) if v.get("type") == "volume"}
    # The render names volumes after its own default project ("docker_..."), shared with any
    # `make e2e-up` cluster; give each fleet project its own.
    volumes = {k: dict(v, name=f"{project}_{k}") for k, v in rendered.get("volumes", {}).items() if k in used}
    networks = {k: dict(v, name=f"{project}_{k}") for k, v in rendered["networks"].items()}
    return dict(services=services, networks=networks, volumes=volumes)


# ---- lifecycle -----------------------------------------------------------------------

def compose_cmd(project, path):
    return ["docker", "compose", "--project-name", project, "-f", path]


def compose_env():
    return dict(os.environ, COMPOSE_DISABLE_ENV_FILE="true")


def project_running(project):
    out = sh(["docker", "ps", "--filter", f"label=com.docker.compose.project={project}", "--format", "{{.Names}}"],
             check=False, capture=True).stdout
    return bool(out.split())


def fleet_containers(inspect_lines, image_ids):
    """(fleet, cpuset) for each `docker inspect` line "<image id>\t<compose project>\t<name>\t<cpuset>"
    whose image ID is a Diesis node image; the fleet is the compose project, or the bare container
    name outside compose."""
    out = []
    for line in inspect_lines.splitlines():
        image, project, name, cpuset = (line.split("\t") + ["", "", ""])[:4]
        if image in image_ids:
            out.append((project if project and project != "<no value>" else name.lstrip("/"), cpuset))
    return out


def fleet_projects(inspect_lines, image_ids):
    return {fleet for fleet, _ in fleet_containers(inspect_lines, image_ids)}


def running_containers():
    """(fleet, cpuset) for every running Diesis validator on this host, any user's, matched by
    image ID because fleets started from an immutable image ID show no tag."""
    ids = set(sh(["docker", "images", "--no-trunc", "--format", "{{.ID}}", "--filter", "reference=diesis-node*"],
                 check=False, capture=True).stdout.split())
    running = sh(["docker", "ps", "-q"], check=False, capture=True).stdout.split()
    if not ids or not running:
        return []
    out = sh(["docker", "inspect", "--format",
              '{{.Image}}\t{{index .Config.Labels "com.docker.compose.project"}}\t{{.Name}}\t{{.HostConfig.CpusetCpus}}',
              *running], check=False, capture=True).stdout
    return fleet_containers(out, ids)


def running_fleets(exclude=()):
    """Every running Diesis fleet on this host, by compose project."""
    return {fleet for fleet, _ in running_containers()} - set(exclude)


def cpuset_cpus(text):
    """The CPUs of a cpuset such as "0-1,4"."""
    cpus = set()
    for part in filter(None, (text or "").split(",")):
        low, _, high = part.partition("-")
        cpus |= set(range(int(low), int(high or low) + 1))
    return cpus


def pinned_cpus(exclude=()):
    """CPUs pinned by the validators of other running Diesis fleets."""
    return set().union(*[cpuset_cpus(cpuset) for fleet, cpuset in running_containers() if fleet not in set(exclude)])


def docker_cpus():
    out = sh(["docker", "info", "--format", "{{.NCPU}}"], check=False, capture=True).stdout.strip()
    return int(out) if out.isdigit() else 0


def choose_cpu_base(busy, cpus, width):
    """First CPU for a fleet pinned to `width` CPUs: 0 unless CPUs other fleets pin overlap it, then
    whichever of 0 and cpus - width overlaps them least (a tie keeps 0)."""
    bases = [0] + ([cpus - width] if cpus > width else [])
    return min(bases, key=lambda base: (len(busy & set(range(base, base + width))), base))


def wait_for_slot(project):
    """With BENCH_DIESIS_MAX_FLEETS=N, start only while fewer than N other Diesis fleets run on
    this host. While the file named by BENCH_DIESIS_HOLD_FILE exists, also wait: creating it yields
    the slot at the next fleet start (a runner's next arm tears its fleet down, then waits here).
    Polls every 30 s for up to BENCH_DIESIS_SLOT_WAIT_S seconds (default 7200)."""
    limit = int(os.environ.get("BENCH_DIESIS_MAX_FLEETS") or 0)
    hold = os.environ.get("BENCH_DIESIS_HOLD_FILE")
    if limit <= 0 and not hold:
        return
    deadline = time.monotonic() + float(os.environ.get("BENCH_DIESIS_SLOT_WAIT_S") or 7200)
    shown = None
    while True:
        held = bool(hold) and Path(hold).exists()
        others = running_fleets(exclude={project}) if limit > 0 else set()
        if not held and (limit <= 0 or len(others) < limit):
            if shown is not None:
                say(f"{project}: fleet slot free ({len(others)} other fleet(s) running)")
            return
        state = (held, others)
        if state != shown:
            say(f"{project}: held by {hold}; waiting until it is removed" if held else
                f"{project}: {len(others)} other Diesis fleets running ({', '.join(sorted(others))}); "
                f"waiting for a slot (limit {limit})")
            shown = state
        if time.monotonic() > deadline:
            raise SystemExit(f"No fleet slot within BENCH_DIESIS_SLOT_WAIT_S (held {held}, running {sorted(others)})")
        time.sleep(30)


def wait_live(block, timeout=180):
    deadline = time.monotonic() + timeout
    first = None
    while time.monotonic() < deadline:
        h = heights(block)
        if first is None and all(x is not None for x in h):
            first = h
        if first and all(x is not None and x > b for x, b in zip(h, first)):
            say(f"live heights {h}")
            return h
        time.sleep(1)
    raise SystemExit(f"Validators did not all advance within {timeout} s (heights {heights(block)})")


def own_other_fleets(project):
    """This user's other running fleets: their fresh fleet, and aged fleets they started."""
    me = fresh_project()
    out = []
    for path in state_dir().glob("*/ports.json"):
        other = path.parent.name
        if other == project or not project_running(other):
            continue
        if other == me or json.loads(path.read_text()).get("owner") == me:
            out.append(other)
    return out


def start(project, aged_tag=None):
    # bench.sh leaves a fleet running after each run; stop this user's other fleet so a
    # fresh run and an aged run do not both load the host. Other users' fleets are untouched.
    for other in own_other_fleets(project):
        say(f"stopping {other} (this user's other fleet)")
        stop_project(other)
    wait_for_slot(project)
    previous = json.loads(ports_path(project).read_text())["block"] if ports_path(project).exists() else None
    own_network = f"{project}_diesis-e2e"
    block = choose_block(previous, busy_host_ports() if not project_running(project) else set(),
                         busy_subnets(own_network))
    repo = checkout()
    ref, image_id = image_ref(repo)
    profile = os.environ.get("BENCH_DIESIS_PROFILE", "capped")
    values = read_profile(profile)
    cpu_base = 0
    if values.get("FLEET_PIN_CPUS") == "true" and values.get("FLEET_CPUS"):
        busy = pinned_cpus(exclude={project})
        cpu_base = choose_cpu_base(busy, docker_cpus(), int(float(values["FLEET_CPUS"])) * len(NODES))
        if busy:
            say(f"{project}: other fleets pin CPUs {sorted(busy)}; pinning validators from CPU {cpu_base}")
    aged_rule = aged_outcome_rule(aged_tag)
    compose, provenance = render(repo, image_id, ref.split(":", 1)[1], profile, project=project, block=block,
                                 outcome_rule=aged_rule or DEFAULT_OUTCOME_RULE, cpu_base=cpu_base)
    if aged_rule and provenance["outcome_signature_rule"] != aged_rule:
        raise SystemExit(f"Aged fleet {aged_tag} has a {aged_rule} genesis, not "
                         f"{provenance['outcome_signature_rule']}")
    directory = state_dir() / project
    directory.mkdir(parents=True, exist_ok=True)
    containers = [compose["services"][n]["container_name"] for n in NODES]
    ports = dict(block, project=project, owner=fresh_project(), containers=containers,
                 node_ips=[f"{block['subnet_prefix']}.{11 + i}" for i in range(len(NODES))], chosen=time.time())
    ports_path(project).write_text(json.dumps(ports, indent=2) + "\n")
    path = directory / "compose.json"
    path.write_text(json.dumps(compose, indent=2) + "\n")
    provenance.update(project=project, image=ref, aged_tag=aged_tag, ports=ports,
                      compose_sha256=hashlib.sha256(path.read_bytes()).hexdigest(), started=time.time())
    (directory / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    say(f"{project}: {ref} ({image_id[:19]}) profile={profile} outcome-rule={provenance['outcome_signature_rule']} "
        f"block {block['block']} (RPC {block['rpc'][0]}, metrics {block['metrics'][0]}, "
        f"subnet {block['subnet_prefix']}.0/24) checkout={repo}")
    sh([*compose_cmd(project, path), "up", "-d", "--no-build", "--pull", "never", "--remove-orphans"],
       env=compose_env(), timeout=600)
    try:
        return wait_live(block)
    except SystemExit:
        capture_failure(directory, compose)
        raise


def capture_failure(directory, compose):
    """Keep each validator's log tail and container state when a fleet does not come up."""
    out = directory / f"failed-start-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    for name in NODES:
        container = compose["services"][name].get("container_name", name)
        logs = sh(["docker", "logs", "--tail", "400", container], check=False, capture=True)
        (out / f"{name}.log").write_text(logs.stdout + logs.stderr)
        state = sh(["docker", "inspect", "--format", "{{json .State}}", container], check=False, capture=True)
        (out / f"{name}.state.json").write_text(state.stdout)
    say(f"validator logs saved to {out}")


def last_compose(project):
    path = state_dir() / project / "compose.json"
    return path if path.exists() else None


def remove_project(project):
    path = last_compose(project)
    if path:
        sh([*compose_cmd(project, path), "down", "--volumes", "--remove-orphans"], check=False, env=compose_env())


def stop_project(project):
    path = last_compose(project)
    if path:
        sh([*compose_cmd(project, path), "stop", "-t", "120"], check=False, env=compose_env())


def cmd_reset(_a):
    remove_project(fresh_project())
    start(fresh_project())


def cmd_up(_a):
    tag = os.environ.get("DIESIS_AGED_TAG")
    if tag:
        manifest(tag)
        start(aged_project(tag), tag)
    else:
        start(fresh_project())


def cmd_down(_a):
    tag = os.environ.get("DIESIS_AGED_TAG")
    if tag:
        stop_project(aged_project(tag))
    else:
        remove_project(fresh_project())


def cmd_stop(_a):
    stop_project(selected_project())


def manifest_path(tag):
    return state_dir() / f"aged-{tag}.json"


def manifest(tag):
    path = manifest_path(tag)
    if not path.exists():
        raise SystemExit(f"No aged fleet {tag} ({path}); build one with `fleet.py age --tag {tag}`")
    return json.loads(path.read_text())


def aged_outcome_rule(tag):
    """The outcome signature rule an aged fleet's genesis was built with, when recorded: every
    later run must keep it, since another rule is another genesis."""
    if not tag or not manifest_path(tag).exists():
        return None
    return manifest(tag)["provenance"].get("outcome_signature_rule")


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


def pool_pending(block):
    total = []
    for port in block["metrics"]:
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
    block = project_ports(project)
    harness = harness_binary()
    urls = ",".join(f"http://127.0.0.1:{p}" for p in block["rpc"])
    env = dict(os.environ, BENCH_KEY=DEV_KEY, BENCH_ASSUME_ISOLATED_BLOCKS="false", BENCH_CONFIRM_WAIT_SECS="5",
               BENCH_PRESIGN_HEADROOM="1.1", BENCH_FUND_CONFIRM_TIMEOUT_SECS="240",
               BENCH_FUND_CHAIN_READY_TIMEOUT_SECS="180", BENCH_FUND_DEPLOY_TIMEOUT_SECS="180")
    out = state_dir() / project
    common = [harness, "--rpc-endpoints", urls, "--validators", urls, "--ws", f"ws://127.0.0.1:{block['ws']}",
              "--chain-id", "19803", "--senders", str(a.senders), "--workers", "64", "--batch-size", "200",
              "--retry-profile", "off", "--finality-confirmations", "0", "--test", "transfer",
              "--bench-name", project, "--quiet"]
    for attempt in range(1, 5):
        target = out / f"fund-{attempt}.json"
        # The 4-transaction check after funding must outlast receipt latency on a busy host.
        result = sh([*common, "--fund", "--execution", "burst", "--txs", "4", "--waves", "1", "--out", target],
                    check=False, env=dict(env, BENCH_CONFIRM_WAIT_SECS="90"), timeout=900)
        if result.returncode == 0 and json.loads(target.read_text())["results"]["confirmed"] == 4:
            break
        time.sleep(30)
    else:
        raise SystemExit("Sender funding failed four times")
    # Load in 600 s chunks until every validator passes --blocks or --max-minutes runs out
    # (sustained mode pre-signs rate x duration, so one run per chunk keeps that bounded). The
    # EVM workload (default) deploys token, pair and NFT contracts (--fund) and mixes transfers,
    # ERC-20, AMM and NFT calls, so state grows as it would under real use.
    def chunk(n):
        load = [*common, "--execution", "sustained", "--duration", 600, "--tps", a.rate,
                "--out", out / f"aging-{n}.json"]
        if a.workload == "evm":
            load[load.index("--test") + 1] = "evm"
            load.append("--fund")
        log = (out / "aging-generator.log").open("a")
        return subprocess.Popen(list(map(str, load)), env=env, stdout=log, stderr=subprocess.STDOUT)

    aging_started = time.time()
    chunks = 1
    gen = chunk(chunks)
    try:
        deadline = time.monotonic() + a.max_minutes * 60
        while True:
            time.sleep(15)
            h = heights(block)
            say(f"aging heights {h}")
            if all(x is not None for x in h) and min(h) >= a.blocks:
                break
            if time.monotonic() > deadline:
                raise SystemExit(f"Aging stopped short at {h} after {a.max_minutes} min")
            if gen.poll() is not None:
                if gen.returncode not in (0, 1) or any(x is None for x in h):
                    raise SystemExit(f"Aging load exited {gen.returncode} at heights {h}")
                chunks += 1
                gen = chunk(chunks)
    finally:
        if gen.poll() is None:
            gen.send_signal(signal.SIGINT)
            try:
                gen.wait(timeout=60)
            except subprocess.TimeoutExpired:
                gen.kill()
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline and max(pool_pending(block) or [0]) > 0:
        time.sleep(5)
    time.sleep(10)
    record = dict(tag=a.tag, project=project, height=min(heights(block)), aging_rate=a.rate, senders=a.senders,
                  workload=a.workload, aging_seconds=time.time() - aging_started, aging_chunks=chunks,
                  genesis_hash=rpc(block["rpc"][0], "eth_getBlockByNumber", ["0x0", False])["hash"],
                  created=time.time(), provenance=json.loads((out / "provenance.json").read_text()))
    stop_project(project)
    manifest_path(a.tag).write_text(json.dumps(record, indent=2) + "\n")
    say(f"aged fleet {a.tag} at height {record['height']}, stopped")


def cmd_destroy_aged(a):
    project = aged_project(a.tag)
    path = last_compose(project)
    if path:
        sh([*compose_cmd(project, path), "down", "--volumes", "--remove-orphans"], env=compose_env())
    # A restored fleet that never started has volumes but no compose file yet.
    leftover = sh(["docker", "volume", "ls", "-q", "--filter", f"label=com.docker.compose.project={project}"],
                  check=False, capture=True).stdout.split()
    if leftover:
        sh(["docker", "volume", "rm", *leftover], check=False, capture=True)
    manifest_path(a.tag).unlink(missing_ok=True)


# ---- saved fixtures ----------------------------------------------------------------------
# A fixture is a tar archive of every volume of a stopped aged fleet (each node's data and
# signing volumes) plus fixture.json, under $BENCH_DIESIS_FIXTURES/<name>, named
# <tag>-<image id 12>-<genesis hash 10>. Restoring extracts the archives into a new aged
# project and never modifies the archives. Note: validators bind their writer-fence journal
# to the datadir's device and inode, and node 6570bb686 refuses a copied datadir at startup
# ("journal datadir identity mismatch"); a restored fixture starts only once that is resolved.

def fixtures_dir():
    path = Path(os.environ.get("BENCH_DIESIS_FIXTURES", state_dir().parent / "fixtures"))
    path.mkdir(parents=True, exist_ok=True)
    return path


def fixture_name(manifest):
    image = manifest["provenance"]["image_id"].removeprefix("sha256:")[:12]
    genesis = manifest["genesis_hash"].removeprefix("0x")[:10]
    return f"{manifest['tag']}-{image}-{genesis}"


def cmd_save_fixture(a):
    """Archive a stopped aged fleet's volumes as a fixture."""
    m = manifest(a.tag)
    project = aged_project(a.tag)
    if project_running(project):
        raise SystemExit(f"{project} is running; stop it first (DIESIS_AGED_TAG={a.tag} fleet.py down)")
    compose = json.loads(last_compose(project).read_text())
    name = fixture_name(m)
    out = fixtures_dir() / name
    if out.exists():
        raise SystemExit(f"Fixture {name} exists at {out}")
    out.mkdir(parents=True)
    image = m["provenance"]["image_id"]
    volumes = {}
    for key, definition in compose["volumes"].items():
        sh(["docker", "run", "--rm", "--network", "none", "--entrypoint", "tar", "-v", f"{definition['name']}:/v:ro",
            "-v", f"{out}:/out", image, "-C", "/v", "-cf", f"/out/{key}.tar", "."], timeout=3600)
        volumes[key] = dict(archive=f"{key}.tar", bytes=(out / f"{key}.tar").stat().st_size)
    record = dict(name=name, tag=a.tag, image=m["provenance"]["image"], image_id=image,
                  genesis_hash=m["genesis_hash"], height=m["height"], workload=m.get("workload"),
                  aging_rate=m.get("aging_rate"), senders=m.get("senders"), aging_seconds=m.get("aging_seconds"),
                  volumes=volumes, total_bytes=sum(v["bytes"] for v in volumes.values()), saved=time.time(),
                  source_manifest=m)
    (out / "fixture.json").write_text(json.dumps(record, indent=2) + "\n")
    say(f"fixture {name}: {record['total_bytes'] / 2**30:.2f} GiB in {out}")
    print(json.dumps(dict(name=name, path=str(out), total_bytes=record["total_bytes"])))


def cmd_restore_fixture(a):
    """Extract a fixture into a new aged fleet <as> (stopped); start it with DIESIS_AGED_TAG=<as>."""
    src = fixtures_dir() / a.fixture
    record = json.loads((src / "fixture.json").read_text())
    project = aged_project(a.as_tag)
    if manifest_path(a.as_tag).exists():
        raise SystemExit(f"Aged fleet {a.as_tag} exists; destroy it first (fleet.py destroy-aged --tag {a.as_tag})")
    for key, info in record["volumes"].items():
        name = f"{project}_{key}"
        if sh(["docker", "volume", "inspect", name], check=False, capture=True).returncode == 0:
            raise SystemExit(f"Volume {name} exists")
        sh(["docker", "volume", "create", "--label", f"com.docker.compose.project={project}",
            "--label", f"com.docker.compose.volume={key}", "--label", f"{OWNER_LABEL}=true", name], capture=True)
        sh(["docker", "run", "--rm", "--network", "none", "--entrypoint", "tar", "-v", f"{name}:/v",
            "-v", f"{src}:/in:ro", record["image_id"], "-C", "/v", "-xpf", f"/in/{info['archive']}"], timeout=3600)
    m = dict(record["source_manifest"], tag=a.as_tag, project=project, restored_from=record["name"],
             restored=time.time())
    manifest_path(a.as_tag).write_text(json.dumps(m, indent=2) + "\n")
    # The restored project starts from the fixture's own compose shape on its next `up`.
    (state_dir() / project).mkdir(parents=True, exist_ok=True)
    say(f"restored {record['name']} as aged fleet {a.as_tag} (height {record['height']}); "
        f"start it with DIESIS_AGED_TAG={a.as_tag}")


def cmd_provenance(_a):
    """Print the selected fleet's provenance, including its port block (bench.sh stores it
    beside each report)."""
    path = state_dir() / selected_project() / "provenance.json"
    print(path.read_text() if path.exists() else "{}")


def cmd_ports(_a):
    """Print the selected fleet's port block (lib.sh reads it)."""
    print(json.dumps(project_ports(selected_project())))


def cmd_shell_env(_a):
    """Shell assignments routing a run through the selected fleet's block (lib.sh evals them)."""
    project = selected_project()
    b = project_ports(project)
    names = b.get("containers") or [f"{project}-{n}" for n in NODES]
    ips = b.get("node_ips") or [f"{b['subnet_prefix']}.{11 + i}" for i in range(len(names))]
    values = dict(
        FLEET_RPC=",".join(f"http://127.0.0.1:{p}" for p in b["rpc"]),
        FLEET_WS=f"ws://127.0.0.1:{b['ws']}",
        FLEET_METRICS=",".join(f"http://127.0.0.1:{p}/metrics" for p in b["metrics"]),
        FLEET_PROMETHEUS=f"http://127.0.0.1:{b['prometheus']}",
        FLEET_CONTAINERS=",".join(names),
        FLEET_BLOCK=str(b["block"]),
        FLEET_TOPOLOGY=" ".join(f"NODE{i + 1}_CONTAINER={n} NODE{i + 1}_IP={ip}"
                                for i, (n, ip) in enumerate(zip(names, ips))),
    )
    print("\n".join(f"{k}={shlex.quote(v)}" for k, v in values.items()))


def cmd_status(_a):
    project = selected_project()
    block = project_ports(project)
    print(json.dumps(dict(project=project, running=project_running(project), block=block["block"],
                          heights=heights(block)), indent=2))


def cmd_render(_a):
    repo = checkout()
    ref, image_id = image_ref(repo)
    rule = aged_outcome_rule(os.environ.get("DIESIS_AGED_TAG")) or DEFAULT_OUTCOME_RULE
    compose, provenance = render(repo, image_id, ref.split(":", 1)[1], os.environ.get("BENCH_DIESIS_PROFILE", "capped"),
                                 project=selected_project(), block=project_ports(selected_project()), outcome_rule=rule)
    print(json.dumps(dict(provenance=provenance, compose=compose), indent=2))


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name in ("reset", "up", "down", "stop", "status", "render", "provenance", "ports", "shell-env"):
        sub.add_parser(name)
    age = sub.add_parser("age")
    age.add_argument("--tag", required=True)
    age.add_argument("--blocks", type=int, default=5100)
    age.add_argument("--rate", type=int, default=400)
    age.add_argument("--senders", type=int, default=512)
    age.add_argument("--max-minutes", type=int, default=360)
    age.add_argument("--workload", choices=["evm", "transfer"], default="evm")
    destroy = sub.add_parser("destroy-aged")
    destroy.add_argument("--tag", required=True)
    save = sub.add_parser("save-fixture")
    save.add_argument("--tag", required=True)
    restore = sub.add_parser("restore-fixture")
    restore.add_argument("--fixture", required=True)
    restore.add_argument("--as", dest="as_tag", required=True)
    a = p.parse_args()
    globals()["cmd_" + a.cmd.replace("-", "_")](a)


if __name__ == "__main__":
    main()
