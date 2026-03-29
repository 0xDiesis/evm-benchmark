#!/usr/bin/env bash
# meta.sh — sourced by benchmark scripts to generate run metadata.
# Usage: source this file, then call generate_meta <output_path> <chain> <mode> <env> <tag>

META_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH_REPO_DIR="$(cd "${META_SCRIPT_DIR}/../.." && pwd)"
HARNESS_CARGO="${BENCH_REPO_DIR}/crates/evm-benchmark/Cargo.toml"

generate_meta() {
    local output_path="$1" chain="$2" mode="$3" env="$4" tag="$5"

    local git_sha git_branch harness_version
    git_sha="$(git -C "${BENCH_REPO_DIR}" rev-parse HEAD 2>/dev/null || echo "unknown")"
    git_branch="$(git -C "${BENCH_REPO_DIR}" branch --show-current 2>/dev/null || echo "unknown")"
    harness_version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "${HARNESS_CARGO}" 2>/dev/null | head -1)"

    python3 -c "
import json, os, subprocess, platform

def env_or(key, default):
    return os.environ.get(key, default)

# --- chain config ---
chain_config = {}
if '${chain}' == 'diesis':
    chain_config = {
        'block_period':            env_or('E2E_BLOCK_PERIOD',          '200ms'),
        'ordering_window':         env_or('E2E_ORDERING_WINDOW',       '20ms'),
        'min_round_delay':         env_or('E2E_MIN_ROUND_DELAY',       '10ms'),
        'max_block_txs':           int(env_or('E2E_MAX_BLOCK_TX_COUNT', '5000')),
        'max_proposal_txs':        int(env_or('E2E_MAX_PROPOSAL_TX_COUNT',  '1024')),
        'max_gas_per_proposal':    int(env_or('E2E_MAX_GAS_PER_PROPOSAL', '30000000')),
        'propagation_delay_stop_threshold': int(env_or('E2E_PROPAGATION_DELAY_STOP_THRESHOLD', '5')),
        'parallel_execution':      env_or('E2E_PARALLEL_EXECUTION',    'full'),
        'commitment_mode':         env_or('E2E_COMMITMENT_MODE',       'verkle'),
        'fec_enabled':             env_or('E2E_FEC_ENABLED',           'true'),
        'fec_redundancy_ratio':    env_or('E2E_FEC_REDUNDANCY_RATIO',  '0.33'),
        'fec_min_message_size':    int(env_or('E2E_FEC_MIN_MESSAGE_SIZE', '1024')),
        'txpool_max_account_slots': int(env_or('E2E_TXPOOL_MAX_ACCOUNT_SLOTS', '5000')),
    }
else:
    chain_config = {
        'chain': '${chain}',
        'node_count': int(env_or('E2E_NODE_COUNT', '1')),
    }

# --- bench params ---
bench_params = {
    'txs':        int(env_or('BENCH_TXS',        '2000')),
    'senders':    int(env_or('BENCH_SENDERS',     '200')),
    'batch_size': int(env_or('BENCH_BATCH_SIZE',  '200')),
    'workers':    int(env_or('BENCH_WORKERS',     '8')),
    'tps':        int(env_or('BENCH_TPS',         '200')),
    'duration':   int(env_or('BENCH_DURATION',    '30')),
}

# --- system info ---
uname_s = platform.system()
uname_m = platform.machine()

try:
    cpus = int(subprocess.check_output(['nproc'], stderr=subprocess.DEVNULL).strip())
except Exception:
    try:
        cpus = int(subprocess.check_output(
            ['sysctl', '-n', 'hw.ncpu'], stderr=subprocess.DEVNULL).strip())
    except Exception:
        cpus = 0

try:
    with open('/proc/meminfo') as f:
        for line in f:
            if line.startswith('MemTotal'):
                mem_kb = int(line.split()[1])
                memory_gb = round(mem_kb / 1024 / 1024, 1)
                break
        else:
            raise FileNotFoundError
except Exception:
    try:
        mem_bytes = int(subprocess.check_output(
            ['sysctl', '-n', 'hw.memsize'], stderr=subprocess.DEVNULL).strip())
        memory_gb = round(mem_bytes / 1024**3, 1)
    except Exception:
        memory_gb = 0

system_info = {
    'os':        uname_s,
    'arch':      uname_m,
    'cpus':      cpus,
    'memory_gb': memory_gb,
}

# --- assemble ---
from datetime import datetime, timezone
meta = {
    'timestamp':       datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
    'chain':           '${chain}',
    'mode':            '${mode}',
    'env':             '${env}',
    'tag':             '${tag}',
    'git_sha':         '${git_sha}',
    'git_branch':      '${git_branch}',
    'harness_version': '${harness_version}',
    'chain_config':    chain_config,
    'bench_params':    bench_params,
    'system':          system_info,
}

with open('${output_path}', 'w') as f:
    json.dump(meta, f, indent=2)
    f.write('\n')
"
    echo "Wrote metadata to ${output_path}"
}
