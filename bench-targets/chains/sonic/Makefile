# Sonic fakenet — 4 validator benchmark target
#
# Quick start:
#   make up          # build + start + connect peers
#   make bench       # run default burst benchmark
#   make down        # stop and clean up

.PHONY: build up down restart connect bench logs status clean

# Build the Sonic Docker image from source
build:
	docker compose build

# Start 4-node fakenet, wait for health, connect peers
up: build
	docker compose up -d
	@echo ""
	@echo "Waiting for nodes to become healthy..."
	@for node in sonic-node-1 sonic-node-2 sonic-node-3 sonic-node-4; do \
		echo -n "  $$node: "; \
		timeout=120; \
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

# Stop all containers
down:
	docker compose down -v --remove-orphans

# Restart from scratch
restart: down up

# Connect peers (if nodes are already running)
connect:
	@bash ./connect-peers.sh

# Deploy MultiSend contract and fund sender accounts
fund:
	@echo "Deploying MultiSend contract..."
	@FUNDER_KEY=0x163f5f0f9a621d72fedd85ffca3d08d131ab4e812181e0d30ffd1c885d20aac7; \
	GAS_PRICE=$$(($$(cast gas-price --rpc-url http://localhost:18545) * 3)); \
	ADDR=$$(forge create --private-key $$FUNDER_KEY --rpc-url http://localhost:18545 \
		--gas-price $$GAS_PRICE --broadcast /tmp/MultiSend.sol:MultiSend 2>/dev/null | grep "Deployed to:" | awk '{print $$3}'); \
	if [ -z "$$ADDR" ]; then \
		echo "  Writing MultiSend source..."; \
		echo 'pragma solidity ^0.8.0; contract MultiSend { function send(address[] calldata to) external payable { uint256 amt = msg.value / to.length; for (uint256 i = 0; i < to.length; i++) { (bool ok,) = to[i].call{value: amt}(""); require(ok); } } }' > /tmp/MultiSend.sol; \
		ADDR=$$(forge create --private-key $$FUNDER_KEY --rpc-url http://localhost:18545 \
			--gas-price $$GAS_PRICE --broadcast /tmp/MultiSend.sol:MultiSend 2>/dev/null | grep "Deployed to:" | awk '{print $$3}'); \
	fi; \
	echo "  MultiSend: $$ADDR"; \
	echo "Generating and funding $(BENCH_SENDERS) sender accounts..."; \
	bash ./fund-senders.sh $(BENCH_SENDERS) $$ADDR

BENCH_SENDERS ?= 200

# Run default burst benchmark (1000 txs)
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
	@for port in 18545 18645 18745 18845; do \
		height=$$(curl -sf http://localhost:$$port \
			-X POST -H "Content-Type: application/json" \
			-d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null | \
			python3 -c "import sys,json; r=json.load(sys.stdin); print(int(r['result'],16))" 2>/dev/null || echo "N/A"); \
		echo "  :$$port -> block $$height"; \
	done

# Full cleanup including Docker image
clean: down
	docker rmi -f sonic-sonic-1 sonic-sonic-2 sonic-sonic-3 sonic-sonic-4 2>/dev/null || true
	rm -f report.json
