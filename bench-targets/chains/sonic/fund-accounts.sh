#!/bin/bash
# Fund N sender accounts on Sonic fakenet using the evm-benchmark signer.
# Uses a small Rust script to generate deterministic keys and sign funding txs.
set -euo pipefail

NUM_ACCOUNTS="${1:-200}"
AMOUNT_ETH="10"  # 10 S per account
RPC_URL="http://localhost:18545"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

# Validator key 1 (pre-funded in fakenet genesis)
FUNDER_KEY="0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7"

echo "Generating and funding ${NUM_ACCOUNTS} sender accounts..."
echo "Using evm-benchmark to sign funding transactions..."

# Generate deterministic keys and fund them using a small inline Rust program
# (avoids Python dependency issues)
# Actually, simpler: use cast from foundry if available, or raw RPC

# Check if cast is available
if command -v cast &>/dev/null; then
    echo "Using foundry's 'cast' for key generation and funding..."

    # Get chain ID
    CHAIN_ID=$(cast chain-id --rpc-url "$RPC_URL" 2>/dev/null)
    echo "Chain ID: $CHAIN_ID"

    # Get gas price (use 2x for safety)
    GAS_PRICE=$(cast gas-price --rpc-url "$RPC_URL" 2>/dev/null)
    GAS_PRICE_2X=$((GAS_PRICE * 2))
    echo "Gas price: $((GAS_PRICE / 1000000000)) gwei (using 2x: $((GAS_PRICE_2X / 1000000000)) gwei)"

    # Get funder nonce
    FUNDER_ADDR=$(cast wallet address "$FUNDER_KEY" 2>/dev/null)
    NONCE=$(cast nonce "$FUNDER_ADDR" --rpc-url "$RPC_URL" 2>/dev/null)
    echo "Funder: $FUNDER_ADDR (nonce: $NONCE)"

    # Generate deterministic keys and fund them
    FUNDED=0
    KEYS=""
    for i in $(seq 0 $((NUM_ACCOUNTS - 1))); do
        # Deterministic key: keccak256("bench-sender-{i}")
        KEY=$(cast keccak "bench-sender-$i" 2>/dev/null)
        ADDR=$(cast wallet address "$KEY" 2>/dev/null)

        if [ -n "$KEYS" ]; then
            KEYS="${KEYS},${KEY}"
        else
            KEYS="${KEY}"
        fi

        # Check balance
        BAL=$(cast balance "$ADDR" --rpc-url "$RPC_URL" 2>/dev/null || echo "0")
        if [ "$BAL" != "0" ] && [ "$(echo "$BAL" | sed 's/[^0-9]//g')" -gt 0 ] 2>/dev/null; then
            continue
        fi

        # Fund the account
        TX_HASH=$(cast send --private-key "$FUNDER_KEY" \
            --rpc-url "$RPC_URL" \
            --gas-price "$GAS_PRICE_2X" \
            --nonce "$NONCE" \
            --value "${AMOUNT_ETH}ether" \
            "$ADDR" 2>/dev/null || echo "FAILED")

        if [ "$TX_HASH" != "FAILED" ]; then
            FUNDED=$((FUNDED + 1))
        fi
        NONCE=$((NONCE + 1))

        if [ $((FUNDED % 50)) -eq 0 ] && [ $FUNDED -gt 0 ]; then
            echo "  Funded $FUNDED / $NUM_ACCOUNTS..."
        fi
    done

    echo ""
    echo "Funded $FUNDED new accounts."
    echo "Writing keys to sender-keys.txt..."
    echo "$KEYS" > "$(dirname "$0")/sender-keys.txt"
    echo "Done! Use with: BENCH_KEY=\$(cat bench-targets/sonic/sender-keys.txt)"

else
    echo "ERROR: 'cast' (foundry) not found."
    echo "Install with: curl -L https://foundry.paradigm.xyz | bash && foundryup"
    echo ""
    echo "Alternative: manually set BENCH_KEY with comma-separated private keys."
    exit 1
fi
