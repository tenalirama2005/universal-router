# universal-router Sprint 4 — multi-process container (v4)
# Runs universal-router on :9009 + four specialist backends internally:
#   :9019 cybergym_agentx (v84)
#   :8080 agentx-osworld
#   :9020 pi_bench_agentx (v49) — new in v4
#   :9021 malt_agentx (v26)   — new in v4
#
# v4 NOTE: switched final image from alpine to debian-bookworm-slim because
# pi_bench_agentx and malt_agentx are dynamically linked against glibc +
# OpenSSL 3, not musl. The router binary is rebuilt for glibc (no
# x86_64-unknown-linux-musl target).

# Stage 1: build universal-router (glibc)
FROM rust:1.94-slim-bookworm AS router-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release

# Stage 2: source for cybergym_agentx binary (already glibc)
FROM tenalirama2026/cybergym-agentx:v84 AS cybergym-source

# Stage 3: source for pi_bench_agentx binary (glibc)
FROM tenalirama2026/pi-bench-agentx:v49 AS pibench-source

# Stage 4: source for malt_agentx binary (glibc)
FROM tenalirama2026/malt-agentx:v26 AS malt-source

# Stage 5: final runtime — debian-slim with glibc + OpenSSL 3
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl libssl3 tini \
    && rm -rf /var/lib/apt/lists/*

# Router binary (newly built for glibc)
COPY --from=router-builder /build/target/release/universal-router /app/universal-router

# CyberGym specialist
COPY --from=cybergym-source /app/cybergym_agentx /app/cybergym_agentx

# Pi-Bench specialist + its ADC credentials JSON
COPY --from=pibench-source /app/pi_bench_agentx /app/pi_bench_agentx
COPY --from=pibench-source /app/adc-credentials.json /app/adc-credentials.json

# MALT specialist
COPY --from=malt-source /app/malt_agentx /app/malt_agentx

# OSWorld specialist (local binary in build context)
COPY agentx-osworld /app/agentx-osworld

RUN chmod +x /app/universal-router /app/cybergym_agentx /app/pi_bench_agentx \
            /app/malt_agentx /app/agentx-osworld

RUN mkdir -p /work && chmod 755 /work

COPY entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

EXPOSE 9009
ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
