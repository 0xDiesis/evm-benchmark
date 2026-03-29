#!/bin/bash
# Connect BSC cluster nodes in a ring topology via admin_addPeer.
# Must be run after all 4 nodes are healthy.
set -euo pipefail

# Docker-internal IPs from docker-compose.yml
NODES=(
    "http://localhost:8545"
    "http://localhost:8645"
    "http://localhost:8745"
    "http://localhost:8845"
)
# Container IPs for enode address rewriting
IPS=(
    "10.102.0.10"
    "10.102.0.11"
    "10.102.0.12"
    "10.102.0.13"
)
NODE_COUNT=${#NODES[@]}

rpc_call() {
    local url="$1"
    local method="$2"
    local params="$3"
    curl -sf "$url" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":${params},\"id\":1}" \
        2>/dev/null
}

echo "Waiting for all nodes to be ready..."
for i in "${!NODES[@]}"; do
    url="${NODES[$i]}"
    attempt=0
    while ! rpc_call "$url" "eth_blockNumber" "[]" > /dev/null 2>&1; do
        attempt=$((attempt + 1))
        if [ $attempt -ge 60 ]; then
            echo "ERROR: Node $((i+1)) at $url not ready after 60 attempts"
            exit 1
        fi
        sleep 1
    done
    echo "  Node $((i+1)) ready"
done

echo ""
echo "Fetching enode addresses..."
ENODES=()
for i in "${!NODES[@]}"; do
    url="${NODES[$i]}"
    enode=$(rpc_call "$url" "admin_nodeInfo" "[]" | \
        python3 -c "import sys,json; print(json.load(sys.stdin)['result']['enode'])" 2>/dev/null)
    # Replace local address with Docker container IP for inter-container connectivity
    enode=$(echo "$enode" | sed "s/@127.0.0.1:/@${IPS[$i]}:/" | sed "s/@0.0.0.0:/@${IPS[$i]}:/")
    ENODES+=("$enode")
    echo "  Node $((i+1)): $enode"
done

echo ""
echo "Connecting nodes in ring topology..."
for ((i=0; i<NODE_COUNT; i++)); do
    for offset in 1 2; do
        j=$(( (i + offset) % NODE_COUNT ))
        peer_enode="${ENODES[$j]}"
        result=$(rpc_call "${NODES[$i]}" "admin_addPeer" "[\"${peer_enode}\"]")
        success=$(echo "$result" | grep -o '"result":true' || true)
        if [ -n "$success" ]; then
            echo "  Node $((i+1)) -> Node $((j+1)): connected"
        else
            echo "  Node $((i+1)) -> Node $((j+1)): $result"
        fi
    done
done

echo ""
echo "Verifying peer counts..."
for i in "${!NODES[@]}"; do
    url="${NODES[$i]}"
    peers=$(rpc_call "$url" "admin_peers" "[]" | \
        python3 -c "import sys,json; print(len(json.load(sys.stdin)['result']))" 2>/dev/null || echo "?")
    echo "  Node $((i+1)): $peers peers"
done

echo ""
echo "BSC cluster ready for benchmarking!"
echo "  RPC: ${NODES[0]}"
echo "  WS:  ws://localhost:8546"
