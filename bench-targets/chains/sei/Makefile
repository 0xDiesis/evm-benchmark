.PHONY: build up down bench status

build:
	docker compose build

up: build
	docker compose up -d
	@echo "Waiting for Sei nodes (this takes ~2 minutes for genesis)..."
	@timeout=180; while [ $$timeout -gt 0 ]; do \
		if curl -sf http://localhost:28545 -X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' >/dev/null 2>&1; then \
			echo "Sei EVM RPC ready!"; break; \
		fi; sleep 3; timeout=$$((timeout - 3)); \
	done

down:
	docker compose down -v --remove-orphans

bench:
	@bash ./run-bench.sh

status:
	@docker compose ps
	@echo ""
	@for port in 28545 28547 28549 28551; do \
		height=$$(curl -sf http://localhost:$$port -X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "N/A"); \
		echo "  :$$port -> block $$height"; \
	done
