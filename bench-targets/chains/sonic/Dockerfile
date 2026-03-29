# Sonic fakenet node — built from source
# Based on https://github.com/0xsoniclabs/sonic/blob/main/Dockerfile

FROM golang:1.25 AS builder

RUN apt-get update && apt-get install -y git musl-dev make

WORKDIR /go/sonic
ARG SONIC_VERSION=v2.1.6
RUN git clone --depth 1 --branch ${SONIC_VERSION} \
    https://github.com/0xsoniclabs/sonic.git . && \
    go mod download && \
    make all

# ---------------------------------------------------------------------------
FROM golang:1.25

RUN apt-get update && apt-get install -y --no-install-recommends \
    iproute2 \
    iputils-ping \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /go/sonic/build/sonicd   /usr/local/bin/
COPY --from=builder /go/sonic/build/sonictool /usr/local/bin/
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 18545 18546 5050 5050/udp

VOLUME /var/sonic

ENTRYPOINT ["/entrypoint.sh"]
