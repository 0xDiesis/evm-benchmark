# Scroll l2geth standalone — single-node benchmark target
#
# Quick start:
#   make up          # start l2geth container
#   make bench       # run default burst benchmark
#   make down        # stop and clean up

.PHONY: up down bench status

# Start Scroll l2geth container
up:
	docker compose up -d
	@echo ""
	@echo "Waiting for Scroll l2geth to become healthy..."
	@timeout=120; \
	while [ $$timeout -gt 0 ]; do \
		if docker inspect --format='{{.State.Health.Status}}' scroll-l2geth 2>/dev/null | grep -q healthy; then \
			echo "  scroll-l2geth: healthy"; \
			break; \
		fi; \
		sleep 2; \
		timeout=$$((timeout - 2)); \
	done; \
	if [ $$timeout -le 0 ]; then echo "  scroll-l2geth: TIMEOUT"; fi

# Stop container and remove volumes
down:
	docker compose down -v --remove-orphans

# Run default burst benchmark (10000 txs)
bench:
	@bash ./run-bench.sh

# Show node status
status:
	@echo "Container status:"
	@docker compose ps
	@echo ""
	@echo "Block height:"
	@height=$$(curl -sf http://localhost:48545 \
		-X POST -H "Content-Type: application/json" \
		-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
		python3 -c "import sys,json; r=json.load(sys.stdin); print(int(r['result'],16))" 2>/dev/null || echo "N/A"); \
	echo "  :48545 -> block $$height"
