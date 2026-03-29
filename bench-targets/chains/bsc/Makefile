# BSC local cluster — 3 validators + 1 RPC benchmark target
#
# Quick start:
#   make up          # build + start + connect peers
#   make bench       # run default burst benchmark
#   make down        # stop and clean up

.PHONY: build up down restart connect bench bench-sustained bench-ceiling logs status clean

# Build the BSC Docker image from source
build:
	docker compose build

# Start cluster, wait for health, connect peers
up: build
	docker compose up -d
	@echo ""
	@echo "Waiting for nodes to become healthy..."
	@for node in bsc-rpc bsc-validator-1 bsc-validator-2 bsc-validator-3; do \
		echo -n "  $$node: "; \
		timeout=180; \
		while [ $$timeout -gt 0 ]; do \
			if docker inspect --format='{{.State.Health.Status}}' $$node 2>/dev/null | grep -q healthy; then \
				echo "healthy"; \
				break; \
			fi; \
			sleep 2; \
			timeout=$$((timeout - 2)); \
		done; \
		if [ $$timeout -le 0 ]; then echo "TIMEOUT"; fi; \
	done
	@echo ""
	@bash ./connect-peers.sh

# Stop all containers and remove volumes
down:
	docker compose down -v --remove-orphans

# Restart from scratch
restart: down up

# Connect peers (if nodes are already running)
connect:
	@bash ./connect-peers.sh

# Run default burst benchmark (10000 txs)
bench:
	@bash ./run-bench.sh

# Run sustained benchmark
bench-sustained:
	@bash ./run-bench.sh --execution sustained --tps 500 --duration 30

# Run ceiling benchmark
bench-ceiling:
	@bash ./run-bench.sh --execution ceiling --tps 200

# Show logs from all nodes
logs:
	docker compose logs -f --tail=50

# Show node status
status:
	@echo "Container status:"
	@docker compose ps
	@echo ""
	@echo "Block heights:"
	@for port in 8545 8645 8745 8845; do \
		height=$$(curl -sf http://localhost:$$port \
			-X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; r=json.load(sys.stdin); print(int(r['result'],16))" 2>/dev/null || echo "N/A"); \
		echo "  :$$port -> block $$height"; \
	done

# Full cleanup including Docker image
clean: down
	docker rmi -f bsc-bsc-rpc bsc-bsc-validator-1 bsc-bsc-validator-2 bsc-bsc-validator-3 2>/dev/null || true
	rm -f report.json
