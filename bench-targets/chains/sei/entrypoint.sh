#!/bin/bash
set -e

NODE_ID="${NODE_ID:-0}"
CLUSTER_SIZE="${CLUSTER_SIZE:-4}"
CHAIN_ID="sei"
HOME_DIR="/root/.sei"
SHARED="/shared"

MONIKER="sei-node-${NODE_ID}"
ACCOUNT_NAME="node_admin"
KEYPASS="12345678"

log() { echo "[node-${NODE_ID}] $*"; }

# -----------------------------------------------------------------------
# 1. Initialize node  (upstream: step1_configure_init.sh)
# -----------------------------------------------------------------------
log "Initializing node..."
seid init "$MONIKER" --chain-id "$CHAIN_ID" --home "$HOME_DIR" >/dev/null 2>&1

# -----------------------------------------------------------------------
# 2. Create validator key  (upstream: step1_configure_init.sh)
# -----------------------------------------------------------------------
log "Creating validator key..."
printf "${KEYPASS}\n${KEYPASS}\ny\n" | seid keys add "$ACCOUNT_NAME" --home "$HOME_DIR" >/dev/null 2>&1 || true

VALIDATOR_ADDR=$(printf "${KEYPASS}\n" | seid keys show "$ACCOUNT_NAME" -a --home "$HOME_DIR")
log "Validator address: $VALIDATOR_ADDR"

# -----------------------------------------------------------------------
# 3. Initial genesis account + gentx  (upstream: step1_configure_init.sh)
#    Upstream uses 3 denoms; amount matches upstream step1.
# -----------------------------------------------------------------------
seid add-genesis-account "$VALIDATOR_ADDR" 10000000usei,10000000uusdc,10000000uatom --home "$HOME_DIR"

printf "${KEYPASS}\n" | seid gentx "$ACCOUNT_NAME" 10000000usei \
    --chain-id "$CHAIN_ID" \
    --home "$HOME_DIR" >/dev/null 2>&1

# -----------------------------------------------------------------------
# 4. Export artifacts to shared volume  (upstream: step1)
# -----------------------------------------------------------------------
mkdir -p "$SHARED/gentx" "$SHARED/nodeids" "$SHARED/addrs"
cp "$HOME_DIR/config/gentx/"*.json "$SHARED/gentx/gentx-${NODE_ID}.json"

seid tendermint show-node-id --home "$HOME_DIR" > "$SHARED/nodeids/node-${NODE_ID}.id"
echo "$VALIDATOR_ADDR" > "$SHARED/addrs/addr-${NODE_ID}.txt"

# Signal this node finished init
touch "$SHARED/init-${NODE_ID}.done"

# -----------------------------------------------------------------------
# 5. Genesis assembly — node 0 only  (upstream: step2_genesis.sh)
# -----------------------------------------------------------------------
if [ "$NODE_ID" = "0" ]; then
    log "Waiting for all nodes to initialize..."
    for i in $(seq 0 $((CLUSTER_SIZE - 1))); do
        while [ ! -f "$SHARED/init-${i}.done" ]; do sleep 0.5; done
    done
    log "All nodes initialized."

    # --- Override genesis parameters (matching upstream step2) ---
    override_genesis() {
        jq "$1" "$HOME_DIR/config/genesis.json" > "$HOME_DIR/config/tmp_genesis.json" \
            && mv "$HOME_DIR/config/tmp_genesis.json" "$HOME_DIR/config/genesis.json"
    }

    override_genesis '.app_state["crisis"]["constant_fee"]["denom"]="usei"'
    override_genesis '.app_state["mint"]["params"]["mint_denom"]="usei"'
    override_genesis '.app_state["staking"]["params"]["bond_denom"]="usei"'
    override_genesis '.app_state["oracle"]["params"]["vote_period"]="2"'
    override_genesis '.app_state["slashing"]["params"]["signed_blocks_window"]="10000"'
    override_genesis '.app_state["slashing"]["params"]["min_signed_per_window"]="0.050000000000000000"'
    override_genesis '.app_state["staking"]["params"]["max_validators"]="50"'
    override_genesis '.consensus_params["block"]["max_gas"]="35000000"'
    override_genesis '.consensus_params["block"]["max_gas_wanted"]="70000000"'
    override_genesis '.app_state["staking"]["params"]["unbonding_time"]="10s"'

    # Token release schedule (upstream step2)
    START_DATE="$(date +"%Y-%m-%d")"
    END_DATE="$(date -d "+3 days" +"%Y-%m-%d" 2>/dev/null || date -v+3d +"%Y-%m-%d")"
    override_genesis ".app_state[\"mint\"][\"params\"][\"token_release_schedule\"]=[{\"start_date\": \"$START_DATE\", \"end_date\": \"$END_DATE\", \"token_release_amount\": \"999999999999\"}]"

    # Clear accounts/balances/gentxs — we re-add all of them below (upstream step2)
    override_genesis '.app_state["auth"]["accounts"]=[]'
    override_genesis '.app_state["bank"]["balances"]=[]'
    override_genesis '.app_state["genutil"]["gen_txs"]=[]'

    # Denom metadata (upstream step2)
    override_genesis '.app_state["bank"]["denom_metadata"]=[{"denom_units":[{"denom":"UATOM","exponent":6,"aliases":["UATOM"]}],"base":"uatom","display":"uatom","name":"UATOM","symbol":"UATOM"}]'

    # Gov parameters (upstream step2)
    override_genesis '.app_state["gov"]["deposit_params"]["min_deposit"][0]["denom"]="usei"'
    override_genesis '.app_state["gov"]["deposit_params"]["min_expedited_deposit"][0]["denom"]="usei"'
    override_genesis '.app_state["gov"]["deposit_params"]["max_deposit_period"]="100s"'
    override_genesis '.app_state["gov"]["voting_params"]["voting_period"]="30s"'
    override_genesis '.app_state["gov"]["voting_params"]["expedited_voting_period"]="15s"'
    override_genesis '.app_state["gov"]["tally_params"]["quorum"]="0.5"'
    override_genesis '.app_state["gov"]["tally_params"]["threshold"]="0.5"'
    override_genesis '.app_state["gov"]["tally_params"]["expedited_quorum"]="0.9"'
    override_genesis '.app_state["gov"]["tally_params"]["expedited_threshold"]="0.9"'

    # --- Re-add ALL genesis accounts with all 3 denoms (upstream step2) ---
    for i in $(seq 0 $((CLUSTER_SIZE - 1))); do
        ADDR=$(cat "$SHARED/addrs/addr-${i}.txt")
        log "Adding genesis account: $ADDR"
        seid add-genesis-account "$ADDR" 1000000000000000000000usei,1000000000000000000000uusdc,1000000000000000000000uatom --home "$HOME_DIR"
    done

    # --- Collect gentxs from all nodes ---
    for i in $(seq 1 $((CLUSTER_SIZE - 1))); do
        cp "$SHARED/gentx/gentx-${i}.json" "$HOME_DIR/config/gentx/" 2>/dev/null || true
    done

    # --- Add validators to genesis BEFORE collect-gentxs (upstream order!) ---
    # upstream: step3_add_validator_to_genesis.sh runs BEFORE seid collect-gentxs
    log "Injecting validators into genesis (upstream step3)..."
    jq '.validators = []' "$HOME_DIR/config/genesis.json" > "$HOME_DIR/config/tmp_genesis.json"

    IDX=0
    for GENTX_FILE in "$HOME_DIR/config/gentx/"*; do
        [ -f "$GENTX_FILE" ] || continue
        KEY=$(jq '.body.messages[0].pubkey.key' "$GENTX_FILE" -c)
        DELEGATION=$(jq -r '.body.messages[0].value.amount' "$GENTX_FILE")
        POWER=$((DELEGATION / 1000000))

        # Build step-by-step exactly like upstream step3
        jq ".validators[$IDX] |= .+ {}" "$HOME_DIR/config/tmp_genesis.json" \
            > "$HOME_DIR/config/tmp_genesis_s1.json"
        jq ".validators[$IDX] += {\"power\":\"$POWER\"}" "$HOME_DIR/config/tmp_genesis_s1.json" \
            > "$HOME_DIR/config/tmp_genesis_s2.json"
        jq ".validators[$IDX] += {\"pub_key\":{\"type\":\"tendermint/PubKeyEd25519\",\"value\":$KEY}}" "$HOME_DIR/config/tmp_genesis_s2.json" \
            > "$HOME_DIR/config/tmp_genesis.json"
        rm -f "$HOME_DIR/config/tmp_genesis_s1.json" "$HOME_DIR/config/tmp_genesis_s2.json"

        IDX=$((IDX + 1))
    done
    mv "$HOME_DIR/config/tmp_genesis.json" "$HOME_DIR/config/genesis.json"
    log "Injected $IDX validator(s) into genesis."

    # --- NOW collect gentxs (upstream order: step3 then collect-gentxs) ---
    log "Collecting gentxs..."
    seid collect-gentxs --home "$HOME_DIR" >/dev/null 2>&1

    # --- Validate genesis ---
    seid validate-genesis --home "$HOME_DIR" || log "WARNING: validate-genesis failed (non-fatal)"

    # --- Distribute assembled genesis ---
    cp "$HOME_DIR/config/genesis.json" "$SHARED/genesis.json"
    touch "$SHARED/genesis-ready"
    log "Genesis distributed."
else
    # Non-leader: wait for assembled genesis from node 0
    log "Waiting for genesis from node 0..."
    while [ ! -f "$SHARED/genesis-ready" ]; do sleep 0.5; done
    cp "$SHARED/genesis.json" "$HOME_DIR/config/genesis.json"
    log "Genesis received."
fi

# -----------------------------------------------------------------------
# 6. Config override  (upstream: step4_config_override.sh)
#    Upstream copies complete config files. We write the critical values
#    matching the upstream docker/localnode/config/ templates exactly.
# -----------------------------------------------------------------------

# --- config.toml overrides ---
CFG="$HOME_DIR/config/config.toml"

# Build persistent peers list
PEERS=""
for i in $(seq 0 $((CLUSTER_SIZE - 1))); do
    if [ "$i" != "$NODE_ID" ]; then
        while [ ! -f "$SHARED/nodeids/node-${i}.id" ]; do sleep 0.5; done
        PEER_ID=$(cat "$SHARED/nodeids/node-${i}.id")
        PEER_HOST="sei-node-${i}"
        if [ -n "$PEERS" ]; then PEERS="${PEERS},"; fi
        PEERS="${PEERS}${PEER_ID}@${PEER_HOST}:26656"
    fi
done

# Persistent peers
sed -i "s|^persistent-peers = .*|persistent-peers = \"${PEERS}\"|" "$CFG"

# Bind addresses for inter-container connectivity
sed -i 's|tcp://127.0.0.1:26656|tcp://0.0.0.0:26656|g' "$CFG"
sed -i 's|tcp://127.0.0.1:26657|tcp://0.0.0.0:26657|g' "$CFG"

# Node mode — MUST be "validator" (upstream config.toml)
sed -i 's|^mode = .*|mode = "validator"|' "$CFG"

# P2P tuning (upstream config.toml)
sed -i 's|^queue-type = .*|queue-type = "priority"|' "$CFG"
sed -i 's|^max-connections = .*|max-connections = 200|' "$CFG"
sed -i 's|^pex = .*|pex = true|' "$CFG"
sed -i 's|^send-rate = .*|send-rate = 204800000|' "$CFG"
sed -i 's|^recv-rate = .*|recv-rate = 204800000|' "$CFG"

# Consensus timeouts — EXACT upstream values from docker/localnode/config/config.toml
sed -i 's|^unsafe-propose-timeout-override = .*|unsafe-propose-timeout-override = "3s"|' "$CFG"
sed -i 's|^unsafe-propose-timeout-delta-override = .*|unsafe-propose-timeout-delta-override = "500ms"|' "$CFG"
sed -i 's|^unsafe-vote-timeout-override = .*|unsafe-vote-timeout-override = "50ms"|' "$CFG"
sed -i 's|^unsafe-vote-timeout-delta-override = .*|unsafe-vote-timeout-delta-override = "500ms"|' "$CFG"
sed -i 's|^unsafe-commit-timeout-override = .*|unsafe-commit-timeout-override = "50ms"|' "$CFG"
sed -i 's|^unsafe-bypass-commit-timeout-override = .*|unsafe-bypass-commit-timeout-override = false|' "$CFG"

# Mempool (upstream)
sed -i 's|^max-txs-bytes = .*|max-txs-bytes = 107374182400|' "$CFG"
sed -i 's|^max-tx-bytes = .*|max-tx-bytes = 20485760|' "$CFG"

# RPC event log (upstream)
sed -i 's|^event-log-window-size = .*|event-log-window-size = "30s"|' "$CFG"

# Instrumentation (upstream)
sed -i 's|^prometheus = .*|prometheus = true|' "$CFG"

# Gossip key only (upstream)
# This may not exist as a top-level key; append if needed
if grep -q "^gossip-tx-key-only" "$CFG"; then
    sed -i 's|^gossip-tx-key-only.*|gossip-tx-key-only=true|' "$CFG"
fi

# --- app.toml overrides ---
APP="$HOME_DIR/config/app.toml"

# Gas prices
sed -i 's|^minimum-gas-prices = .*|minimum-gas-prices = "0.01usei"|' "$APP"

# Pruning (upstream: nothing)
sed -i 's|^pruning = .*|pruning = "nothing"|' "$APP"

# Retain blocks (upstream)
sed -i 's|^min-retain-blocks = .*|min-retain-blocks = 4000|' "$APP"

# Concurrency (upstream)
sed -i 's|^concurrency-workers = .*|concurrency-workers = 4|' "$APP"
sed -i 's|^occ-enabled = .*|occ-enabled = true|' "$APP"

# API (upstream)
sed -i 's|^enable = .*# Enable defines if the API|enable = true # Enable defines if the API|' "$APP"
# Simpler: just set the enabled-unsafe-cors
sed -i 's|^enabled-unsafe-cors = .*|enabled-unsafe-cors = true|' "$APP"

# Telemetry (upstream)
if grep -q "^prometheus-retention-time" "$APP"; then
    sed -i 's|^prometheus-retention-time = .*|prometheus-retention-time = 5|' "$APP"
fi

# SeiDB State Commit (upstream app.toml — critical for Sei v6)
if grep -q "^sc-enable" "$APP"; then
    sed -i 's|^sc-enable = .*|sc-enable = true|' "$APP"
    sed -i 's|^sc-async-commit-buffer = .*|sc-async-commit-buffer = 100|' "$APP"
    sed -i 's|^sc-keep-recent = .*|sc-keep-recent = 1|' "$APP"
    sed -i 's|^sc-snapshot-interval = .*|sc-snapshot-interval = 1000|' "$APP"
    sed -i 's|^sc-cache-size = .*|sc-cache-size = 1000|' "$APP"
fi

# SeiDB State Store (upstream app.toml)
if grep -q "^ss-enable" "$APP"; then
    sed -i 's|^ss-enable = .*|ss-enable = true|' "$APP"
    sed -i 's|^ss-backend = .*|ss-backend = "pebbledb"|' "$APP"
    sed -i 's|^ss-async-write-buffer = .*|ss-async-write-buffer = 100|' "$APP"
    sed -i 's|^ss-keep-recent = .*|ss-keep-recent = 10000|' "$APP"
fi

# Enable slow mode (upstream step4)
if grep -q "^slow = " "$APP"; then
    sed -i 's|^slow = .*|slow = true|' "$APP"
fi

# EVM JSON-RPC
sed -i 's|^rpc_http_address = .*|rpc_http_address = "0.0.0.0:8545"|' "$APP"
sed -i 's|^rpc_ws_address = .*|rpc_ws_address = "0.0.0.0:8546"|' "$APP"

# EVM test API (upstream)
if grep -q "^enable_test_api" "$APP"; then
    sed -i 's|^enable_test_api = .*|enable_test_api = true|' "$APP"
fi

# -----------------------------------------------------------------------
# 7. Start seid  (upstream: step5_start_sei.sh)
# -----------------------------------------------------------------------
log "Starting seid..."
exec seid start --chain-id "$CHAIN_ID" --home "$HOME_DIR"
