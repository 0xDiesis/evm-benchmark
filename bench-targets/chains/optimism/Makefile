# Optimism (Supersim) — OP Stack benchmark target
#
# Quick start:
#   make up          # start supersim (mock L1 + L2)
#   make bench       # run default burst benchmark against L2
#   make down        # stop and clean up

.PHONY: up down bench bench-sustained bench-ceiling status clean

# Start supersim via docker-compose, wait for L2 RPC health
up:
	docker compose up -d
	@echo ""
	@echo "Waiting for Supersim L2 to become healthy..."
	@timeout=60; \
	while [ $$timeout -gt 0 ]; do \
		if curl -sf http://127.0.0.1:9545 -X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' > /dev/null 2>&1; then \
			echo "  L2 RPC (port 9545): healthy"; \
			break; \
		fi; \
		sleep 2; \
		timeout=$$((timeout - 2)); \
	done; \
	if [ $$timeout -le 0 ]; then echo "  L2 RPC: TIMEOUT"; exit 1; fi
	@echo ""
	@echo "Supersim is ready. L2 RPC: http://127.0.0.1:9545"

# Stop all containers
down:
	docker compose down -v --remove-orphans

# Run default burst benchmark (1000 txs)
bench:
	@bash ./run-bench.sh

# Run sustained benchmark
bench-sustained:
	@bash ./run-bench.sh --execution sustained --tps 500 --duration 30

# Run ceiling benchmark
bench-ceiling:
	@bash ./run-bench.sh --execution ceiling --tps 200

# Show node status
status:
	@echo "Container status:"
	@docker compose ps
	@echo ""
	@echo "L2 block height:"
	@height=$$(curl -sf http://127.0.0.1:9545 \
		-X POST -H "Content-Type: application/json" \
		-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
		python3 -c "import sys,json; r=json.load(sys.stdin); print(int(r['result'],16))" 2>/dev/null || echo "N/A"); \
	echo "  :9545 -> block $$height"

# Full cleanup including Docker image
clean: down
	docker rmi -f ethereum-optimism/supersim:latest 2>/dev/null || true
	rm -f report.json
