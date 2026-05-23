#!/bin/sh
# Multi-process entrypoint: cybergym_agentx on :9019, then router on :9000.
# NOTE: uses 127.0.0.1 (explicit IPv4) — in Alpine, "localhost" resolves to
# IPv6 ::1, but cybergym_agentx binds IPv4 0.0.0.0 only.
set -e

ROUTER_PORT="${ROUTER_PORT:-9000}"
CYBERGYM_PORT="${CYBERGYM_PORT:-9019}"

echo "[entrypoint] starting cybergym_agentx (v83) on :${CYBERGYM_PORT}..."
(
    export PORT="${CYBERGYM_PORT}"
    export AGENT_URL="http://127.0.0.1:${CYBERGYM_PORT}"
    exec /app/cybergym_agentx
) &
CYBERGYM_PID=$!

RETRY=0
MAX_RETRY=30
HEALTHY=0
while [ $RETRY -lt $MAX_RETRY ]; do
    BODY="$(wget -q -O - "http://127.0.0.1:${CYBERGYM_PORT}/health" 2>/dev/null || true)"
    if [ -n "$BODY" ]; then
        echo "[entrypoint] cybergym_agentx healthy after ${RETRY}s: ${BODY}"
        HEALTHY=1
        break
    fi
    RETRY=$((RETRY + 1))
    sleep 1
done

if [ "$HEALTHY" -ne 1 ]; then
    echo "[entrypoint] ERROR: cybergym_agentx failed health check in ${MAX_RETRY}s"
    kill $CYBERGYM_PID 2>/dev/null
    exit 1
fi

OSWORLD_PORT="${OSWORLD_PORT:-8080}"
echo "[entrypoint] starting agentx-osworld on :${OSWORLD_PORT}..."
(
    export PORT="${OSWORLD_PORT}"
    export AGENT_URL="http://127.0.0.1:${OSWORLD_PORT}"
    export NEBIUS_API_KEY="${NEBIUS_API_KEY}"
    export QWEN_API_BASE_URL="https://api.tokenfactory.nebius.com/v1"
    export QWEN_MODEL="Qwen/Qwen2.5-VL-72B-Instruct"
    export JEDI_API_BASE_URL="https://api.tokenfactory.nebius.com/v1"
    export JEDI_MODEL="Qwen/Qwen2.5-VL-72B-Instruct"
    exec /app/agentx-osworld
) &
OSWORLD_PID=$!

RETRY=0; HEALTHY=0
while [ $RETRY -lt 30 ]; do
    BODY="$(wget -q -O - "http://127.0.0.1:${OSWORLD_PORT}/health" 2>/dev/null || true)"
    if [ -n "$BODY" ]; then
        echo "[entrypoint] agentx-osworld healthy after ${RETRY}s: ${BODY}"
        HEALTHY=1; break
    fi
    RETRY=$((RETRY + 1)); sleep 1
done
if [ "$HEALTHY" -ne 1 ]; then
    echo "[entrypoint] ERROR: agentx-osworld failed health check"
    kill $OSWORLD_PID 2>/dev/null; exit 1
fi

echo "[entrypoint] starting universal-router on :${ROUTER_PORT}..."
export PORT="${ROUTER_PORT}"
export UPSTREAM_VULN_REPRO="http://127.0.0.1:${CYBERGYM_PORT}"
export UPSTREAM_GUI_AGENT="http://127.0.0.1:${OSWORLD_PORT}"
exec /app/universal-router
