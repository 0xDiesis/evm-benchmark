FROM golang:1.25-bookworm AS builder
ARG AVAGO_VERSION=v1.14.2
WORKDIR /build
RUN git clone --depth 1 --branch ${AVAGO_VERSION} https://github.com/ava-labs/avalanchego.git . && \
    go mod download && \
    ./scripts/build.sh

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl iproute2 iputils-ping && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/build/avalanchego /usr/local/bin/
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
EXPOSE 9650 9651
ENTRYPOINT ["/entrypoint.sh"]
