#!/bin/bash
# entrypoint.sh — Initialize and run a single-validator Evmos node for benchmarking.
# Based on evmos/local_node.sh — uses evmosd init + evmosd start (one process).
# JSON-RPC on 0.0.0.0:8545, chain ID evmos_9000-1.
set -euo pipefail

CHAINID="${CHAIN_ID:-evmos_9000-1}"
BASE_DENOM="aevmos"
MONIKER="bench-node"
KEYRING="test"
KEYALGO="eth_secp256k1"
LOGLEVEL="info"
HOMEDIR="${HOME}/.evmosd-bench"
BASEFEE=1000000000

CONFIG="${HOMEDIR}/config/config.toml"
APP_TOML="${HOMEDIR}/config/app.toml"
GENESIS="${HOMEDIR}/config/genesis.json"
TMP_GENESIS="${HOMEDIR}/config/tmp_genesis.json"

rm -rf "${HOMEDIR}"

# validator key: address 0x7cb61d4117ae31a12e393a1cfa3bac666481d02e
VAL_KEY="mykey"
VAL_MNEMONIC="gesture inject test cycle original hollow east ridge hen combine junk child bacon zero hope comfort vacuum milk pitch cage oppose unhappy lunar seat"

# bench-funder: address 0xc6fe5d33615a1c52c08018c47e8bc53646a0e101
USER1_KEY="dev0"
USER1_MNEMONIC="copper push brief egg scan entry inform record adjust fossil boss egg comic alien upon aspect dry avoid interest fury window hint race symptom"

# dev1 address 0x963ebdf2e1f8db8707d05fc75bfeffba1b5bac17
USER2_KEY="dev1"
USER2_MNEMONIC="maximum display century economy unlock van census kite error heart snow filter midnight usage egg venture cash kick motor survey drastic edge muffin visual"

# dev2 address 0x40a0cb1C63e026A81B55EE1308586E21eec1eFa9
USER3_KEY="dev2"
USER3_MNEMONIC="will wear settle write dance topic tape sea glory hotel oppose rebel client problem era video gossip glide during yard balance cancel file rose"

# dev3 address 0x498B5AeC5D439b733dC2F58AB489783A23FB26dA
USER4_KEY="dev3"
USER4_MNEMONIC="doll midnight silk carpet brush boring pluck office gown inquiry duck chief aim exit gain never tennis crime fragile ship cloud surface exotic patch"

echo "Importing keys..."
evmosd config set client chain-id "$CHAINID" --home "$HOMEDIR"
evmosd config set client keyring-backend "$KEYRING" --home "$HOMEDIR"

echo "$VAL_MNEMONIC"   | evmosd keys add "$VAL_KEY"   --recover --keyring-backend "$KEYRING" --algo "$KEYALGO" --home "$HOMEDIR"
echo "$USER1_MNEMONIC" | evmosd keys add "$USER1_KEY" --recover --keyring-backend "$KEYRING" --algo "$KEYALGO" --home "$HOMEDIR"
echo "$USER2_MNEMONIC" | evmosd keys add "$USER2_KEY" --recover --keyring-backend "$KEYRING" --algo "$KEYALGO" --home "$HOMEDIR"
echo "$USER3_MNEMONIC" | evmosd keys add "$USER3_KEY" --recover --keyring-backend "$KEYRING" --algo "$KEYALGO" --home "$HOMEDIR"
echo "$USER4_MNEMONIC" | evmosd keys add "$USER4_KEY" --recover --keyring-backend "$KEYRING" --algo "$KEYALGO" --home "$HOMEDIR"

echo "Initializing chain..."
evmosd init "$MONIKER" -o --chain-id "$CHAINID" --home "$HOMEDIR"

# Set denomination
jq --arg d "$BASE_DENOM" '.app_state["staking"]["params"]["bond_denom"]=$d' "$GENESIS" >"$TMP_GENESIS" && mv "$TMP_GENESIS" "$GENESIS"
jq --arg d "$BASE_DENOM" '.app_state["gov"]["deposit_params"]["min_deposit"][0]["denom"]=$d' "$GENESIS" >"$TMP_GENESIS" && mv "$TMP_GENESIS" "$GENESIS"
jq --arg d "$BASE_DENOM" '.app_state["gov"]["params"]["min_deposit"][0]["denom"]=$d' "$GENESIS" >"$TMP_GENESIS" && mv "$TMP_GENESIS" "$GENESIS"
jq --arg d "$BASE_DENOM" '.app_state["inflation"]["params"]["mint_denom"]=$d' "$GENESIS" >"$TMP_GENESIS" && mv "$TMP_GENESIS" "$GENESIS"

# Gas limit and base fee
jq '.consensus_params["block"]["max_gas"]="10000000"' "$GENESIS" >"$TMP_GENESIS" && mv "$TMP_GENESIS" "$GENESIS"
jq ".app_state[\"feemarket\"][\"params\"][\"base_fee\"]=\"${BASEFEE}\"" "$GENESIS" >"$TMP_GENESIS" && mv "$TMP_GENESIS" "$GENESIS"

# Fund accounts generously (100k EVMOS each for bench senders)
FUND_AMOUNT="100000000000000000000000${BASE_DENOM}"
evmosd add-genesis-account "$VAL_KEY"   "$FUND_AMOUNT" --keyring-backend "$KEYRING" --home "$HOMEDIR"
evmosd add-genesis-account "$USER1_KEY" "$FUND_AMOUNT" --keyring-backend "$KEYRING" --home "$HOMEDIR"
evmosd add-genesis-account "$USER2_KEY" "$FUND_AMOUNT" --keyring-backend "$KEYRING" --home "$HOMEDIR"
evmosd add-genesis-account "$USER3_KEY" "$FUND_AMOUNT" --keyring-backend "$KEYRING" --home "$HOMEDIR"
evmosd add-genesis-account "$USER4_KEY" "$FUND_AMOUNT" --keyring-backend "$KEYRING" --home "$HOMEDIR"

# Stake for the validator
evmosd gentx "$VAL_KEY" "1000000000000000000000${BASE_DENOM}" \
    --keyring-backend "$KEYRING" \
    --chain-id "$CHAINID" \
    --home "$HOMEDIR"

evmosd collect-gentxs --home "$HOMEDIR"
evmosd validate-genesis --home "$HOMEDIR"

# Enable JSON-RPC and configure listen addresses
sed -i 's/enable = false/enable = true/g' "$APP_TOML"
sed -i 's/enabled = false/enabled = true/g' "$APP_TOML"
# Disable non-essential APIs
grep -q '\[rosetta\]'  "$APP_TOML" && sed -i '/\[rosetta\]/,/^\[/ s/enable = true/enable = false/' "$APP_TOML"
grep -q '\[memiavl\]'  "$APP_TOML" && sed -i '/\[memiavl\]/,/^\[/ s/enable = true/enable = false/' "$APP_TOML"
grep -q '\[versiondb\]' "$APP_TOML" && sed -i '/\[versiondb\]/,/^\[/ s/enable = true/enable = false/' "$APP_TOML"

# Bind JSON-RPC to all interfaces
sed -i 's|address = "127.0.0.1:8545"|address = "0.0.0.0:8545"|g' "$APP_TOML"
sed -i 's|ws-address = "127.0.0.1:8546"|ws-address = "0.0.0.0:8546"|g' "$APP_TOML"

echo "Starting Evmos node..."
exec evmosd start \
    --home "$HOMEDIR" \
    --chain-id "$CHAINID" \
    --minimum-gas-prices "0.0001${BASE_DENOM}" \
    --json-rpc.address "0.0.0.0:8545" \
    --json-rpc.ws-address "0.0.0.0:8546" \
    --log_level "$LOGLEVEL"
