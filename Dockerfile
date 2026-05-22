# universal-router Sprint 4 — multi-process container
# Runs universal-router on :9000 + cybergym_agentx (v83) on :9019 internally.

# Stage 1: build universal-router
FROM rust:1.94-alpine AS router-builder
RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release --target x86_64-unknown-linux-musl

# Stage 2: source for cybergym_agentx binary
FROM tenalirama2026/cybergym-agentx:v83 AS cybergym-source

# Stage 3: final runtime
FROM alpine:3.20
RUN apk add --no-cache ca-certificates tini
COPY --from=router-builder /build/target/x86_64-unknown-linux-musl/release/universal-router /app/universal-router
COPY --from=cybergym-source /app/cybergym_agentx /app/cybergym_agentx
COPY agentx-osworld /app/agentx-osworld
RUN chmod +x /app/agentx-osworld
RUN mkdir -p /work && chmod 755 /work
COPY entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh
EXPOSE 9000
ENTRYPOINT ["/sbin/tini", "--", "/app/entrypoint.sh"]
