#!/bin/bash
# kurtosis-start.sh — Start Kurtosis with Docker credential workaround and retry logic.
#
# Two issues this script handles:
#
# 1. Docker credential metacharacters: Kurtosis engine copies credentials into
#    a container volume using `printf`, which fails when credentials contain
#    shell metacharacters (&, |, ^, %, etc.). Private-registry credentials
#    (e.g. ECR tokens) almost always contain these characters. The fix is to run
#    Kurtosis with a temporary Docker config that has no auths or credential
#    helpers. This target only needs local/public images after the local beacond
#    image is built, so registry credentials are unnecessary and risky here.
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

KURTOSIS_DOCKER_CONFIG="$(mktemp -d "${TMPDIR:-/tmp}/bera-kurtosis-docker-config.XXXXXX")"
cleanup() {
    rm -rf "${KURTOSIS_DOCKER_CONFIG}"
    [[ -n "${KURTOSIS_RUN_LOG:-}" ]] && rm -f "${KURTOSIS_RUN_LOG}"
}
trap cleanup EXIT INT TERM

printf '{"auths":{}}\n' > "${KURTOSIS_DOCKER_CONFIG}/config.json"
export DOCKER_CONFIG="${KURTOSIS_DOCKER_CONFIG}"
echo "Using temporary Docker config for Kurtosis: ${DOCKER_CONFIG}"

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

run_kurtosis() {
    kurtosis run "${BEACON_KIT_DIR}/kurtosis" --args-file "${SCRIPT_DIR}/kurtosis-config.yaml" \
        --enclave "${ENCLAVE}" --parallelism 200
}

KURTOSIS_RUN_LOG="$(mktemp "${TMPDIR:-/tmp}/bera-kurtosis-run.XXXXXX")"
if ! run_kurtosis 2>&1 | tee "${KURTOSIS_RUN_LOG}"; then
    if grep -q "No logs aggregator container exists" "${KURTOSIS_RUN_LOG}"; then
        echo "Kurtosis engine is missing its logs aggregator; restarting engine and retrying once..." >&2
        kurtosis engine stop >/dev/null 2>&1 || true
        sleep 2
        kurtosis engine start
        run_kurtosis
    else
        exit 1
    fi
fi
