FROM golang:1.25-bookworm AS builder
ARG SEI_VERSION=v6.4.0
WORKDIR /build
RUN apt-get update && apt-get install -y git build-essential
RUN git clone --depth 1 --branch ${SEI_VERSION} --recurse-submodules \
    https://github.com/sei-protocol/sei-chain.git .
# Build with mock_balances tag for auto-funding
RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=1 go build -tags "netgo ledger mock_balances" \
    -ldflags "-w -s" \
    -o /usr/local/bin/seid ./cmd/seid
# Copy ALL wasmvm libraries (match build architecture)
RUN ARCH=$(uname -m) && \
    find . -name "libwasmvm*${ARCH}*" -exec cp {} /usr/local/lib/ \; 2>/dev/null && \
    find . -name "libwasmvm*.so" -exec cp {} /usr/local/lib/ \; 2>/dev/null && \
    ldconfig && \
    ls -la /usr/local/lib/libwasmvm*

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl jq iproute2 iputils-ping && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/seid /usr/local/bin/
COPY --from=builder /usr/local/lib/libwasmvm* /usr/local/lib/
RUN ldconfig
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
EXPOSE 26656 26657 26658 8545 8546 9090
ENTRYPOINT ["/entrypoint.sh"]
