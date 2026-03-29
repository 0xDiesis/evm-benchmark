# Evmos v20.0.0 in-process 4-validator testnet image.
# Uses `evmosd testnet start` which runs all validators in a single process —
# no multi-container coordination required.
# No host Go installation required.

# ── build stage ───────────────────────────────────────────────────────────────
FROM golang:1.23.4-alpine3.20 AS build-env

ARG EVMOS_TAG=v20.0.0

RUN apk add --no-cache \
    ca-certificates \
    build-base \
    git \
    linux-headers \
    bash \
    binutils-gold

WORKDIR /build
RUN git clone --depth 1 --branch ${EVMOS_TAG} \
    https://github.com/evmos/evmos.git .

RUN go mod download
RUN COSMOS_BUILD_OPTIONS=goleveldb make build

# ── runtime stage ─────────────────────────────────────────────────────────────
FROM alpine:3.21

RUN apk add --no-cache \
    ca-certificates \
    bash \
    curl \
    jq \
    python3 \
    iproute2 \
    iputils-ping

COPY --from=build-env /build/build/evmosd /usr/bin/evmosd
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# EVM JSON-RPC
EXPOSE 8545 8546
# Tendermint RPC
EXPOSE 26657

HEALTHCHECK --interval=5s --timeout=3s --start-period=30s --retries=30 \
    CMD curl -sf http://localhost:8545 -X POST \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        | grep -q '"result"' || exit 1

CMD ["/usr/local/bin/entrypoint.sh"]
