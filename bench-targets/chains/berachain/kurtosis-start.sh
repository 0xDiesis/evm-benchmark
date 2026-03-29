#!/bin/bash
# kurtosis-start.sh — Start Kurtosis with Docker credential workaround and retry logic.
#
# Two issues this script handles:
#
# 1. Docker credential metacharacters: Kurtosis engine copies credentials into
#    a container volume using `printf`, which fails when credentials contain
#    shell metacharacters (&, |, ^, %, etc.). Private-registry credentials
#    (e.g. ECR tokens) almost always contain these characters. The fix is to
#    strip all `auths` entries whose auth/password fields contain metacharacters,
#    keeping only safe entries (typically credHelpers/credsStore which Kurtosis
#    doesn't need inside the container anyway).
#
# 2. Docker Desktop socket transient: On macOS, Docker Desktop's Unix socket
#    briefly returns EOF for the very first container start after a cold start
#    or restart. The container actually starts fine, but Kurtosis sees the EOF
#    and aborts. Retrying resolves this.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENCLAVE="${1:-bera-bench}"
BEACON_KIT_DIR="${SCRIPT_DIR}/.beacon-kit"
MAX_ENGINE_RETRIES=4

DOCKER_CFG="${HOME}/.docker/config.json"
DOCKER_CFG_BAK="${HOME}/.docker/config.json.kurtosis-bak"

restore_docker_config() {
    if [[ -f "${DOCKER_CFG_BAK}" ]]; then
        cp "${DOCKER_CFG_BAK}" "${DOCKER_CFG}"
        rm -f "${DOCKER_CFG_BAK}"
    fi
}
trap restore_docker_config EXIT INT TERM

# Build a sanitized Docker config that strips any auth entries whose credentials
# contain shell metacharacters that break Kurtosis's printf-into-container approach.
# ECR tokens (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY encoded in auth) almost
# always contain &, +, =, /, ^ etc. We keep credHelpers and credsStore intact
# since Kurtosis doesn't try to embed those — only `auths` entries are copied.
if [[ -f "${DOCKER_CFG}" ]]; then
    cp "${DOCKER_CFG}" "${DOCKER_CFG_BAK}"
    python3 - "${DOCKER_CFG}" <<'PYEOF'
import json, sys, re

METACHAR_RE = re.compile(r'[&|^%!<>(){}$`\\]')

with open(sys.argv[1]) as f:
    cfg = json.load(f)

auths = cfg.get("auths", {})
safe_auths = {}
for registry, creds in auths.items():
    auth_val = creds.get("auth", "")
    password = creds.get("password", "")
    if METACHAR_RE.search(auth_val) or METACHAR_RE.search(password):
        print(f"  Stripping auth entry for {registry} (contains shell metacharacters)", file=sys.stderr)
        continue
    safe_auths[registry] = creds

cfg["auths"] = safe_auths

with open(sys.argv[1], "w") as f:
    json.dump(cfg, f, indent=2)

print(f"  Sanitized Docker config: kept {len(safe_auths)}/{len(auths)} auth entries", file=sys.stderr)
PYEOF
    echo "Temporarily sanitized Docker config (will restore on exit)"
fi

# Ensure Kurtosis engine is running (with retry for Docker socket transients)
engine_started=false
for attempt in $(seq 1 "${MAX_ENGINE_RETRIES}"); do
    if kurtosis engine status 2>/dev/null | grep -q "running"; then
        echo "Kurtosis engine already running"
        engine_started=true
        break
    fi

    # Clean up stale containers from previous failed attempts
    stale=$(docker ps -a --filter "name=logs-aggregator" --format "{{.ID}}" 2>/dev/null)
    [[ -n "${stale}" ]] && docker rm -f ${stale} > /dev/null 2>&1 || true

    echo "Starting Kurtosis engine (attempt ${attempt}/${MAX_ENGINE_RETRIES})..."
    if kurtosis engine start 2>&1; then
        engine_started=true
        break
    fi

    if [[ "${attempt}" -lt "${MAX_ENGINE_RETRIES}" ]]; then
        echo "Engine start failed, retrying in 3s..."
        sleep 3
    fi
done

if [[ "${engine_started}" != "true" ]]; then
    echo "ERROR: Kurtosis engine failed to start after ${MAX_ENGINE_RETRIES} attempts" >&2
    exit 1
fi

kurtosis run "${BEACON_KIT_DIR}/kurtosis" --args-file "${SCRIPT_DIR}/kurtosis-config.yaml" \
    --enclave "${ENCLAVE}" --parallelism 200
