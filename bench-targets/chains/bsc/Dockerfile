# BSC geth v1.7.2 — built from source (works on ARM64 and x86_64)
#
# Multi-stage build: compile in golang:1.25-alpine, run in alpine:3.21

FROM golang:1.25-alpine AS builder

RUN apk add --no-cache gcc g++ musl-dev git linux-headers make bash

WORKDIR /go/bsc
ARG BSC_VERSION=v1.7.2
RUN git clone --depth 1 --branch ${BSC_VERSION} \
    https://github.com/bnb-chain/bsc.git . && \
    go mod download && \
    make geth

# ---------------------------------------------------------------------------
FROM alpine:3.21

RUN apk add --no-cache bash iproute2 iputils-ping ca-certificates

COPY --from=builder /go/bsc/build/bin/geth /usr/local/bin/geth
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 8545 8546 30311 30311/udp

VOLUME /var/bsc

ENTRYPOINT ["/entrypoint.sh"]
