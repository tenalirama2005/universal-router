#!/bin/sh
# Universal Router v4 — multi-process entrypoint.
#
# Internal port layout:
#   :9009  universal-router (public)
#   :9019  cybergym_agentx
#   :8080  agentx-osworld
#   :9020  pi_bench_agentx        (Pi-Bench)
#   :9021  malt_agentx            (MALT / NetArena)
#
# Each backend uses 127.0.0.1 explicitly — Alpine→Debian migration retained
# this discipline because some Rust agents bind IPv4 0.0.0.0 only.
set -e

ROUTER_PORT="${ROUTER_PORT:-9009}"
CYBERGYM_PORT="${CYBERGYM_PORT:-9019}"
OSWORLD_PORT="${OSWORLD_PORT:-8080}"
PIBENCH_PORT="${PIBENCH_PORT:-9020}"
MALT_PORT_INTERNAL="${MALT_PORT_INTERNAL:-9021}"

# ── helper: wait for a TCP service to return non-empty HTTP body on /health ──
wait_health() {
    name="$1"
    port="$2"
    pid="$3"
    max="${4:-30}"
    i=0
    while [ "$i" -lt "$max" ]; do
        body="$(curl -s -m 1 "http://127.0.0.1:${port}/health" 2>/dev/null || true)"
        if [ -n "$body" ]; then
            echo "[entrypoint] ${name} healthy after ${i}s: ${body}"
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    echo "[entrypoint] ERROR: ${name} failed /health check in ${max}s"
    kill "$pid" 2>/dev/null
    return 1
}

# ── helper: wait for /.well-known/agent-card.json (for agents without /health) ─
wait_card() {
    name="$1"
    port="$2"
    pid="$3"
    max="${4:-30}"
    i=0
    while [ "$i" -lt "$max" ]; do
        body="$(curl -s -m 1 "http://127.0.0.1:${port}/.well-known/agent-card.json" 2>/dev/null || true)"
        if echo "$body" | grep -q '"name"'; then
            echo "[entrypoint] ${name} agent-card responding after ${i}s"
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    echo "[entrypoint] ERROR: ${name} failed agent-card check in ${max}s"
    kill "$pid" 2>/dev/null
    return 1
}

# ─── 1) CyberGym ──────────────────────────────────────────────────────────────
echo "[entrypoint] starting cybergym_agentx (v84) on :${CYBERGYM_PORT}..."
(
    export PORT="${CYBERGYM_PORT}"
    export AGENT_URL="http://127.0.0.1:${CYBERGYM_PORT}"
    exec /app/cybergym_agentx
) &
CYBERGYM_PID=$!
wait_health "cybergym_agentx" "${CYBERGYM_PORT}" "${CYBERGYM_PID}" 30 || exit 1

# ─── 2) OSWorld ───────────────────────────────────────────────────────────────
echo "[entrypoint] starting agentx-osworld on :${OSWORLD_PORT}..."
(
    export PORT="${OSWORLD_PORT}"
    export AGENT_URL="http://127.0.0.1:${OSWORLD_PORT}"
    export NEBIUS_API_KEY="${NEBIUS_API_KEY}"
    export NVIDIA_API_KEY="${NVIDIA_API_KEY}"
    export QWEN_API_BASE_URL="https://api.studio.nebius.com/v1"
    export QWEN_MODEL="Qwen/Qwen2.5-VL-72B-Instruct"
    export JEDI_API_BASE_URL="https://api.studio.nebius.com/v1"
    export JEDI_MODEL="Qwen/Qwen2.5-VL-72B-Instruct"
    exec /app/agentx-osworld
) &
OSWORLD_PID=$!
wait_health "agentx-osworld" "${OSWORLD_PORT}" "${OSWORLD_PID}" 30 || exit 1

# ─── 3) Pi-Bench ──────────────────────────────────────────────────────────────
echo "[entrypoint] starting pi_bench_agentx (v49) on :${PIBENCH_PORT}..."
(
    # Pi-Bench uses --port flag (default is 8766). Env vars below mirror
    # the deployed pi-bench-agentx amber-manifest config_schema.
    export AZURE_OPENAI_KEY="${PIBENCH_AZURE_OPENAI_KEY}"
    export AZURE_OPENAI_ENDPOINT="${PIBENCH_AZURE_OPENAI_ENDPOINT:-https://pi-bench-agentx-resource.cognitiveservices.azure.com/}"
    export GOOGLE_APPLICATION_CREDENTIALS=/app/adc-credentials.json
    exec /app/pi_bench_agentx --host 127.0.0.1 --port "${PIBENCH_PORT}"
) &
PIBENCH_PID=$!
# Pi-Bench's lightweight endpoint is agent-card not /health
wait_card "pi_bench_agentx" "${PIBENCH_PORT}" "${PIBENCH_PID}" 30 || exit 1

# ─── 4) MALT / NetArena ───────────────────────────────────────────────────────
echo "[entrypoint] starting malt_agentx (v26) on :${MALT_PORT_INTERNAL}..."
(
    # MALT respects PORT env var only — confirmed via probes:
    #   PORT=9021 → "Starting on port 9021"      ✓ works
    #   --port 9021 → terminated (unknown flag)  ✗ ignored
    # MALT also binds 0.0.0.0 by default; no --host flag needed.
    export PORT="${MALT_PORT_INTERNAL}"
    export AZURE_OPENAI_KEY="${MALT_AZURE_OPENAI_KEY}"
    export AZURE_OPENAI_ENDPOINT="${MALT_AZURE_OPENAI_ENDPOINT:-https://malt-agentx-resource.cognitiveservices.azure.com/}"
    exec /app/malt_agentx
) &
MALT_PID=$!
wait_card "malt_agentx" "${MALT_PORT_INTERNAL}" "${MALT_PID}" 30 || exit 1

# ─── 5) Universal Router (foreground) ─────────────────────────────────────────
echo "[entrypoint] all specialists healthy — starting universal-router on :${ROUTER_PORT}..."
export PORT="${ROUTER_PORT}"
export UPSTREAM_VULN_REPRO="http://127.0.0.1:${CYBERGYM_PORT}"
export UPSTREAM_GUI_AGENT="http://127.0.0.1:${OSWORLD_PORT}"
export UPSTREAM_POLICY_TOOLUSE="http://127.0.0.1:${PIBENCH_PORT}"
export UPSTREAM_TEXT_CODEGEN="http://127.0.0.1:${MALT_PORT_INTERNAL}"
# UPSTREAM_VISION_QA remains unset until FWA backend is bundled (Day 2+)
exec /app/universal-router
