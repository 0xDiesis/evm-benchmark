.PHONY: build up down bench status

build:
	docker compose build

up: build
	docker compose up -d
	@echo "Waiting for Avalanche nodes..."
	@for node in avax-node-1 avax-node-2 avax-node-3 avax-node-4 avax-node-5; do \
		printf "  $$node: "; timeout=120; \
		while [ $$timeout -gt 0 ]; do \
			if [ "$$node" = "avax-node-1" ]; then \
				if docker inspect --format='{{.State.Health.Status}}' $$node 2>/dev/null | grep -q healthy; then \
					echo "healthy"; break; \
				fi; \
			else \
				if docker inspect --format='{{.State.Status}}' $$node 2>/dev/null | grep -q running; then \
					echo "running"; break; \
				fi; \
			fi; \
			sleep 2; timeout=$$((timeout - 2)); \
		done; \
		if [ $$timeout -le 0 ]; then echo "TIMEOUT"; fi; \
	done

down:
	docker compose down -v --remove-orphans

bench:
	@bash ./run-bench.sh

status:
	@docker compose ps
	@echo ""
	@for port in 9650 9660 9670 9680 9690; do \
		height=$$(curl -sf http://localhost:$$port/ext/bc/C/rpc -X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "N/A"); \
		echo "  :$$port -> C-Chain block $$height"; \
	done
