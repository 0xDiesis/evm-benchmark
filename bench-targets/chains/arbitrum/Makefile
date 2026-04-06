# Arbitrum Nitro testnode — L2 optimistic rollup benchmark target
#
# Quick start:
#   make up          # clone nitro-testnode (if needed), init + start, wait for health
#   make bench       # run default burst benchmark against L2 sequencer
#   make down        # stop and clean up
#   make status      # check L2 block height

.PHONY: up down bench bench-sustained bench-ceiling status logs clean

NITRO_DIR := ./nitro-testnode

# Clone nitro-testnode if not present, initialize and start in detached mode,
# then wait for the L2 sequencer RPC to become healthy.
up:
	@if [ ! -d "$(NITRO_DIR)" ]; then \
		echo "Cloning nitro-testnode..."; \
		git clone --depth 1 https://github.com/OffchainLabs/nitro-testnode.git $(NITRO_DIR); \
	fi
	@echo "Starting Arbitrum Nitro testnode..."
	cd $(NITRO_DIR) && ./test-node.bash --init --detach
	@echo ""
	@echo "Waiting for L2 sequencer to become healthy..."
	@timeout=180; elapsed=0; \
	while [ $$elapsed -lt $$timeout ]; do \
		if curl -sf http://127.0.0.1:8547 -X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' > /dev/null 2>&1; then \
			echo "  L2 sequencer (port 8547): healthy"; \
			break; \
		fi; \
		sleep 2; \
		elapsed=$$((elapsed + 2)); \
	done; \
	if [ $$elapsed -ge $$timeout ]; then \
		echo "  L2 sequencer: TIMEOUT after $${timeout}s"; \
		exit 1; \
	fi

# Stop all containers
down:
	@if [ -d "$(NITRO_DIR)" ]; then \
		cd $(NITRO_DIR) && docker compose down -v --remove-orphans 2>/dev/null || true; \
	fi

# Run default burst benchmark (1000 txs)
bench:
	@bash ./run-bench.sh

# Run sustained benchmark
bench-sustained:
	@bash ./run-bench.sh --execution sustained --tps 500 --duration 30

# Find TPS ceiling
bench-ceiling:
	@bash ./run-bench.sh --execution ceiling --tps 200

# Show L2 sequencer status
status:
	@echo "L2 Sequencer:"
	@height=$$(curl -sf http://127.0.0.1:8547 \
		-X POST -H "Content-Type: application/json" \
		-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
		python3 -c "import sys,json; r=json.load(sys.stdin); print(int(r['result'],16))" 2>/dev/null || echo "N/A"); \
	echo "  :8547 -> block $$height"
	@echo ""
	@echo "Container status:"
	@if [ -d "$(NITRO_DIR)" ]; then \
		cd $(NITRO_DIR) && docker compose ps 2>/dev/null || echo "  Not running"; \
	else \
		echo "  nitro-testnode not cloned"; \
	fi

# Show logs
logs:
	@if [ -d "$(NITRO_DIR)" ]; then \
		cd $(NITRO_DIR) && docker compose logs -f --tail=50; \
	fi

# Full cleanup including cloned repo
clean: down
	rm -rf $(NITRO_DIR)
	rm -f report.json
