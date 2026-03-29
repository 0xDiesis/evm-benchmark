#!/bin/sh
set -e

DATADIR="/var/sonic"
VALIDATOR_ID="${VALIDATOR_ID:-1}"
VALIDATOR_COUNT="${VALIDATOR_COUNT:-4}"

# Initialize genesis if not already done
if [ ! -d "${DATADIR}/chaindata" ]; then
    echo "Initializing fakenet genesis: validator ${VALIDATOR_ID}/${VALIDATOR_COUNT}"
    sonictool --datadir="${DATADIR}" genesis fake "${VALIDATOR_COUNT}" --mode=rpc
fi

# Resolve container hostname to IP for --nat flag (Sonic requires a raw IP, not hostname)
if [ -n "${NAT_HOST:-}" ]; then
    NAT_IP=$(getent hosts "${NAT_HOST}" 2>/dev/null | awk '{print $1}' || echo "")
    if [ -z "$NAT_IP" ]; then
        NAT_IP=$(hostname -i 2>/dev/null | awk '{print $1}' || echo "0.0.0.0")
    fi
else
    NAT_IP=$(hostname -i 2>/dev/null | awk '{print $1}' || echo "0.0.0.0")
fi

echo "Starting sonicd: validator ${VALIDATOR_ID}/${VALIDATOR_COUNT} (nat=${NAT_IP})"
exec sonicd \
    --datadir="${DATADIR}" \
    --fakenet "${VALIDATOR_ID}/${VALIDATOR_COUNT}" \
    --mode rpc \
    --cache "${CACHE_SIZE:-6144}" \
    --port "${P2P_PORT:-5050}" \
    --nat "extip:${NAT_IP}" \
    --http \
    --http.addr 0.0.0.0 \
    --http.port "${HTTP_PORT:-18545}" \
    --http.corsdomain "*" \
    --http.api "eth,debug,net,admin,web3,personal,txpool,dag" \
    --ws \
    --ws.addr 0.0.0.0 \
    --ws.port "${WS_PORT:-18546}" \
    --ws.origins "*" \
    --ws.api "eth,debug,net,admin,web3,personal,txpool,dag" \
    --txpool.globalslots "${TXPOOL_GLOBAL_SLOTS:-5000}" \
    --txpool.accountslots "${TXPOOL_ACCOUNT_SLOTS:-256}" \
    --txpool.globalqueue "${TXPOOL_GLOBAL_QUEUE:-2000}" \
    --txpool.accountqueue "${TXPOOL_ACCOUNT_QUEUE:-128}" \
    --verbosity "${VERBOSITY:-3}" \
    "$@"
