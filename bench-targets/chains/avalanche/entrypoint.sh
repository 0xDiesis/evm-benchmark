#!/bin/sh
set -e

NODE_ID="${NODE_ID:-1}"
DATA_DIR="/data/avalanche"
mkdir -p "$DATA_DIR"

# Resolve hostname to IP (Avalanche requires a raw IP, not hostname)
if [ -n "${PUBLIC_IP:-}" ]; then
    RESOLVED_IP=$(getent hosts "${PUBLIC_IP}" 2>/dev/null | awk '{print $1}' || echo "")
    if [ -z "$RESOLVED_IP" ]; then
        RESOLVED_IP=$(hostname -i 2>/dev/null | awk '{print $1}' || echo "127.0.0.1")
    fi
else
    RESOLVED_IP=$(hostname -i 2>/dev/null | awk '{print $1}' || echo "127.0.0.1")
fi

# Base args for local network
ARGS="--network-id=local"
ARGS="$ARGS --http-host=0.0.0.0"
ARGS="$ARGS --http-port=9650"
ARGS="$ARGS --staking-port=9651"
ARGS="$ARGS --data-dir=$DATA_DIR"
ARGS="$ARGS --sybil-protection-enabled=false"
ARGS="$ARGS --staking-ephemeral-cert-enabled=true"
ARGS="$ARGS --log-level=info"
ARGS="$ARGS --public-ip=${RESOLVED_IP}"

# Bootstrap from node 1 if not the bootstrap node
if [ -n "${BOOTSTRAP_IP:-}" ]; then
    ARGS="$ARGS --bootstrap-ips=${BOOTSTRAP_IP}:9651"
fi

echo "Starting avalanchego node ${NODE_ID} (ip=${RESOLVED_IP})..."
exec avalanchego $ARGS
