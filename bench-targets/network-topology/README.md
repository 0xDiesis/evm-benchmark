# Network Topology Simulation

Benchmark chains under realistic network conditions by simulating geographic
validator distribution. Instead of running all nodes on the same Docker bridge
with zero latency, this injects **pairwise latency** between specific node pairs
so that co-located validators see 2ms while intercontinental pairs see 200ms+.

This matters because most blockchain benchmarks are run on localhost and report
numbers that will never be achieved in production. Consensus protocols are
fundamentally latency-bound — a validator can't vote on a block it hasn't
received yet. By simulating real network distances, we get throughput numbers
that actually predict production performance.

## Why Pairwise Latency (Not Flat Delays)

Most chaos testing tools (Pumba, Comcast, toxiproxy) add a flat delay to a
container's network interface. That means node-3 gets 80ms to *everyone* — even
a node sitting right next to it. Real networks don't work like that.

We use Linux TC (`tc netem` + `u32` destination-IP filters) to apply **different
delays to each peer**. This is the only way to model a network where:

- US-East ↔ US-West = 60ms
- US-East ↔ EU = 90ms
- US-East ↔ Asia = 180ms
- EU ↔ Asia = 240ms

...all simultaneously, on the same container.

```
Container eth0
┌─────────────────────────────────┐
│  prio qdisc (root)              │
│  ├── band 1: default (no delay) │  ← host traffic, DNS
│  ├── band 2: netem 30ms ±4ms    │──► u32: dst 10.100.0.12 (US-West)
│  ├── band 3: netem 45ms ±7ms    │──► u32: dst 10.100.0.13 (EU)
│  └── band 4: netem 90ms ±14ms   │──► u32: dst 10.100.0.14 (Asia)
└─────────────────────────────────┘
```

Each side applies half the RTT. Jitter is 15% of the one-way delay with a normal
distribution, closely matching real-world WAN variance.

## Quick Start

```bash
# From this repo:
./bench-targets/network-topology/network-topology.sh apply global-spread
./bench-targets/network-topology/network-topology.sh verify global-spread
./bench-targets/network-topology/network-topology.sh clear
```

For other chains, override the container names and IPs:

```bash
NODE1_CONTAINER=sonic-node-1 NODE1_IP=10.101.0.11 \
NODE2_CONTAINER=sonic-node-2 NODE2_IP=10.101.0.12 \
NODE3_CONTAINER=sonic-node-3 NODE3_IP=10.101.0.13 \
NODE4_CONTAINER=sonic-node-4 NODE4_IP=10.101.0.14 \
./bench-targets/network-topology/network-topology.sh apply global-spread
```

## Geographic Layouts

Five layouts based on real-world submarine cable and terrestrial backbone
measurements. Each defines a full pairwise RTT matrix.

### `global-spread` — Worldwide (60-240ms)

Validators in US-East, US-West, EU-Frankfurt, Asia-Tokyo. The most common
real-world deployment pattern for globally distributed validator sets.

```
              US-East   US-West   EU-Frank  Asia-Tokyo
US-East       —         60ms      90ms      180ms
US-West       60ms      —         140ms     120ms
EU-Frankfurt  90ms      140ms     —         240ms
Asia-Tokyo    180ms     120ms     240ms     —
```

### `us-distributed` — Continental (20-60ms)

Four US regions. Represents a validator set deployed within a single continent
where latencies are moderate and relatively uniform.

```
              US-East   US-West   US-Central US-South
US-East       —         60ms      30ms       35ms
US-West       60ms      —         40ms       45ms
US-Central    30ms      40ms      —          20ms
US-South      35ms      45ms      20ms       —
```

### `eu-cluster` — Co-located + Outlier (2-90ms)

Three validators in the same datacenter region, one remote. Tests how a
consensus protocol handles asymmetric connectivity — the majority can reach
quorum fast but must wait for the outlier to finalize.

```
              EU-1    EU-2    EU-3    US-East
EU-Frank-1    —       2ms     2ms     90ms
EU-Frank-2    2ms     —       2ms     90ms
EU-Frank-3    2ms     2ms     —       90ms
US-East       90ms    90ms    90ms    —
```

### `intercontinental` — 4 Continents (75-340ms)

Worst-case geographic spread: US-East, EU-London, Asia-Singapore, South
America-Sao Paulo. Includes the notoriously slow Asia ↔ South America link
(340ms RTT via transpacific cables).

```
              US-East  EU-London Singapore Sao Paulo
US-East       —        75ms      230ms     130ms
EU-London     75ms     —         180ms     190ms
Singapore     230ms    180ms     —         340ms
Sao Paulo     130ms    190ms     340ms     —
```

### `degraded-wan` — Congested Network (80-200ms)

All links have high baseline latency with heavy jitter. Simulates peak
internet traffic, DDoS mitigation, or poor ISP peering. Useful as a stress
test — if your protocol works here, it works anywhere.

```
              A       B       C       D
Degraded-A    —       80ms    120ms   200ms
Degraded-B    80ms    —       100ms   180ms
Degraded-C    120ms   100ms   —       150ms
Degraded-D    200ms   180ms   150ms   —
```

## Supported Chains

| Chain | Status | Container IPs | Notes |
|-------|--------|---------------|-------|
| Diesis | Ready | 10.100.0.11-14 | Default target |
| Sonic | Ready | 10.101.0.11-14 | Override env vars needed |
| Avalanche | Ready | 10.0.30.2-6 | 5 nodes (script uses first 4) |
| Sei | Ready | 192.168.20.10-13 | Override env vars needed |
| BSC | Ready | 10.102.0.10-13 | 3 validators + 1 RPC node |
| Cosmos (Evmos) | N/A | — | Single-validator node; no inter-validator consensus latency to simulate |
| Berachain | Not supported | — | Uses Kurtosis, not Compose |

All supported chains have `iproute2` + `iputils-ping` in their Docker images
and `NET_ADMIN` capability in their compose files.

## Cross-Chain Comparison

Run the same workloads on multiple chains under identical network conditions:

```bash
./bench-targets/network-topology/run-comparison-bench.sh global-spread
```

This script:
1. Starts each configured chain one at a time
2. Applies the same topology profile for each run
3. Runs burst/sustained/ceiling benchmarks with identical parameters
4. Writes per-chain reports and a normalized comparison summary

## How It Works

1. **Static IPs** — Each node gets a fixed IP in the Docker Compose network
   (`ipam` subnet + `ipv4_address` per service)

2. **NET_ADMIN capability** — Containers can manipulate their own network stack

3. **`iproute2` in the image** — Provides the `tc` command for traffic control

4. **Per-destination netem** — The script creates a `prio` qdisc with one band
   per peer, each with its own `netem delay` and a `u32` filter matching the
   peer's destination IP. Band 1 (default) passes non-peer traffic with no delay.

5. **Split RTT** — Each side applies half the round-trip delay. Node A adds 30ms
   toward node B, and node B adds 30ms toward node A = 60ms RTT.

6. **Normal distribution jitter** — 15% of the one-way delay. So 30ms ± 4.5ms
   means ~68% of packets arrive within 25.5-34.5ms, ~95% within 21-39ms.

## Commands

```bash
network-topology.sh apply <layout>      # Apply pairwise latency
network-topology.sh clear               # Remove all tc rules
network-topology.sh status              # Show tc qdiscs and filters per node
network-topology.sh verify [<layout>]   # Ping all pairs, show expected vs actual
network-topology.sh layouts             # List available layouts with RTT ranges
```

## Requirements

- Docker with Docker Compose v2
- Linux kernel with `sch_netem` module (standard on Docker Desktop and Linux)
- Container images with `iproute2` installed
- `NET_ADMIN` capability on target containers
- Static IPs assigned in the Docker network
