.PHONY: up down bench status deps build-image clean

ENCLAVE := bera-bench
BEACON_KIT_DIR := .beacon-kit
BEACON_KIT_REPO := https://github.com/berachain/beacon-kit.git
BEACON_KIT_TAG  := v1.3.7

# Clone beacon-kit at pinned release (needed for Kurtosis package)
$(BEACON_KIT_DIR):
	git clone --depth 1 --branch $(BEACON_KIT_TAG) $(BEACON_KIT_REPO) $(BEACON_KIT_DIR)

deps:
	@command -v kurtosis >/dev/null 2>&1 || { echo "ERROR: kurtosis CLI not found. Install: brew install kurtosis-tech/tap/kurtosis-cli"; exit 1; }
	@command -v docker >/dev/null 2>&1 || { echo "ERROR: docker not found"; exit 1; }

build-image: $(BEACON_KIT_DIR)
	@echo "Building beacond Docker image (this takes a few minutes)..."
	cd $(BEACON_KIT_DIR) && $(MAKE) build-docker VERSION=kurtosis-local

up: deps build-image
	@echo "Starting Berachain Kurtosis devnet (enclave: $(ENCLAVE))..."
	@# Remove stale enclave if it exists
	@kurtosis enclave rm -f $(ENCLAVE) 2>/dev/null || true
	@# kurtosis-start.sh handles the Docker credential workaround:
	@# Kurtosis engine uses printf to copy credentials into a container volume,
	@# which fails when credentials contain shell metacharacters. The script
	@# temporarily clears ~/.docker/config.json and restores it on exit.
	@bash ./kurtosis-start.sh $(ENCLAVE)
	@echo ""
	@echo "Berachain devnet ready. Discovering RPC ports..."
	@bash ./discover-ports.sh

down:
	kurtosis enclave rm -f $(ENCLAVE) 2>/dev/null || true

bench:
	@bash ./run-bench.sh

status:
	@kurtosis enclave inspect $(ENCLAVE) 2>/dev/null || echo "Enclave $(ENCLAVE) not running"
	@echo ""
	@bash ./discover-ports.sh 2>/dev/null || true
	@echo ""
	@RPC=$$(kurtosis port print $(ENCLAVE) el-validator-reth-0 eth-json-rpc 2>/dev/null) && \
		height=$$(curl -sf "$$RPC" -X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "N/A") && \
		echo "  el-validator-reth-0 -> block $$height" || true

clean: down
	rm -rf $(BEACON_KIT_DIR)
