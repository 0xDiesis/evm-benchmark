.PHONY: build up down bench status clean

# Evmos v20.0.0 in-process 4-validator testnet benchmark target.
# All validators run in a single container via `evmosd testnet start`.
# JSON-RPC on localhost:8545, chain ID 9000.
# No host Go installation required — evmosd is built inside Docker.

build:
	docker compose build

up: build
	docker compose up -d
	@echo ""
	@echo "Waiting for Evmos EVM RPC (http://localhost:8545)..."
	@for i in $$(seq 1 90); do \
		if curl -sf http://localhost:8545 -X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' >/dev/null 2>&1; then \
			echo "Evmos EVM RPC ready."; break; \
		fi; \
		sleep 2; \
	done

down:
	docker compose down -v --remove-orphans

bench:
	@bash ./run-bench.sh

status:
	@docker compose ps
	@echo ""
	@curl -sf http://localhost:8545 -X POST -H "Content-Type: application/json" \
		-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
		python3 -c "import sys,json; print('Block:', int(json.load(sys.stdin)['result'],16))" 2>/dev/null || echo "RPC not responding"

clean: down
	docker rmi -f cosmos-bench-evmos-node 2>/dev/null || true
