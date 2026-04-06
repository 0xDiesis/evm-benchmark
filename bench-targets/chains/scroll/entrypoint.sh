#!/bin/bash
set -e

DATADIR="/var/scroll"
NETWORK_ID="${NETWORK_ID:-53077}"
SIGNER_KEY="${SIGNER_KEY:-}"

# Initialize genesis if not already done
if [ ! -d "${DATADIR}/geth/chaindata" ]; then
    echo "[entrypoint] Initializing Scroll l2geth genesis..."
    geth init --datadir "${DATADIR}" /genesis.json
fi

# Import signer key if provided
if [ -n "${SIGNER_KEY}" ] && [ ! -f "${DATADIR}/.key-imported" ]; then
    echo "[entrypoint] Importing signer key..."
    echo "${SIGNER_KEY}" > /tmp/key.hex
    echo "" > /tmp/password.txt
    geth account import --datadir "${DATADIR}" --password /tmp/password.txt /tmp/key.hex
    rm -f /tmp/key.hex
    touch "${DATADIR}/.key-imported"
fi

# Get the signer address from the imported key
SIGNER_ADDR=$(geth account list --datadir "${DATADIR}" 2>/dev/null | head -1 | sed -n 's/.*{\([0-9a-fA-F]*\)}.*/\1/p')
if [ -z "${SIGNER_ADDR}" ]; then
    echo "[entrypoint] ERROR: No account found for signer!"
    exit 1
fi

echo "[entrypoint] Starting Scroll l2geth with signer: 0x${SIGNER_ADDR}"
echo "" > /tmp/password.txt

exec geth \
    --datadir "${DATADIR}" \
    --networkid "${NETWORK_ID}" \
    --port 30311 \
    --http \
    --http.addr 0.0.0.0 \
    --http.port 8545 \
    --http.corsdomain "*" \
    --http.vhosts "*" \
    --http.api "eth,net,web3,txpool,debug,admin,personal" \
    --ws \
    --ws.addr 0.0.0.0 \
    --ws.port 8546 \
    --ws.origins "*" \
    --ws.api "eth,net,web3,txpool,debug,admin" \
    --txpool.globalslots 50000 \
    --txpool.accountslots 5000 \
    --txpool.globalqueue 10000 \
    --txpool.accountqueue 1000 \
    --syncmode full \
    --gcmode archive \
    --verbosity 3 \
    --allow-insecure-unlock \
    --nodiscover \
    --mine \
    --miner.etherbase "0x${SIGNER_ADDR}" \
    --unlock "0x${SIGNER_ADDR}" \
    --password /tmp/password.txt \
    "$@"
