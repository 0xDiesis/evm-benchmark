#!/bin/bash
set -e

DATADIR="/var/bsc"
NODE_ROLE="${NODE_ROLE:-rpc}"          # "validator" or "rpc"
VALIDATOR_KEY="${VALIDATOR_KEY:-}"     # hex private key (without 0x prefix)
NETWORK_ID="${NETWORK_ID:-714714}"

# Initialize genesis if not already done
if [ ! -d "${DATADIR}/geth/chaindata" ]; then
    echo "[entrypoint] Initializing genesis for ${NODE_ROLE} node..."
    geth init --datadir "${DATADIR}" /genesis.json
fi

# Import validator key if provided
if [ -n "${VALIDATOR_KEY}" ] && [ ! -f "${DATADIR}/.key-imported" ]; then
    echo "[entrypoint] Importing validator key..."
    echo "${VALIDATOR_KEY}" > /tmp/key.hex
    echo "" > /tmp/password.txt
    geth account import --datadir "${DATADIR}" --password /tmp/password.txt /tmp/key.hex
    rm -f /tmp/key.hex
    touch "${DATADIR}/.key-imported"
fi

# Resolve NAT IP from container hostname
NAT_IP=$(hostname -i 2>/dev/null | awk '{print $1}' || echo "0.0.0.0")

# Common flags
COMMON_FLAGS=(
    --datadir "${DATADIR}"
    --networkid "${NETWORK_ID}"
    --port "${P2P_PORT:-30311}"
    --nat "extip:${NAT_IP}"
    --cache "${CACHE_SIZE:-4096}"
    --http
    --http.addr 0.0.0.0
    --http.port "${HTTP_PORT:-8545}"
    --http.corsdomain "*"
    --http.vhosts "*"
    --http.api "eth,net,web3,txpool,parlia,debug,admin,personal"
    --ws
    --ws.addr 0.0.0.0
    --ws.port "${WS_PORT:-8546}"
    --ws.origins "*"
    --ws.api "eth,net,web3,txpool,parlia,debug,admin"
    --txpool.globalslots "${TXPOOL_GLOBAL_SLOTS:-10000}"
    --txpool.accountslots "${TXPOOL_ACCOUNT_SLOTS:-1000}"
    --txpool.globalqueue "${TXPOOL_GLOBAL_QUEUE:-5000}"
    --txpool.accountqueue "${TXPOOL_ACCOUNT_QUEUE:-500}"
    --syncmode full
    --gcmode archive
    --verbosity "${VERBOSITY:-3}"
    --allow-insecure-unlock
    --nodiscover
)

if [ "${NODE_ROLE}" = "validator" ]; then
    # Get the validator address from the imported key
    VALIDATOR_ADDR=$(geth account list --datadir "${DATADIR}" 2>/dev/null | head -1 | sed -n 's/.*{\([0-9a-fA-F]*\)}.*/\1/p')
    if [ -z "${VALIDATOR_ADDR}" ]; then
        echo "[entrypoint] ERROR: No account found for validator!"
        exit 1
    fi
    echo "[entrypoint] Starting validator: 0x${VALIDATOR_ADDR} (nat=${NAT_IP})"
    echo "" > /tmp/password.txt
    exec geth "${COMMON_FLAGS[@]}" \
        --mine \
        --miner.etherbase "0x${VALIDATOR_ADDR}" \
        --unlock "0x${VALIDATOR_ADDR}" \
        --password /tmp/password.txt \
        "$@"
else
    echo "[entrypoint] Starting RPC node (nat=${NAT_IP})"
    exec geth "${COMMON_FLAGS[@]}" "$@"
fi
