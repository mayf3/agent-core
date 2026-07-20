#!/bin/bash
# =============================================================================
# shadow-supervisor.sh — Shadow Canary Process Supervisor
#
# Runs INSIDE the Lima VM in a single persistent limactl shell session.
# All services share the same process group and survive for the duration
# of inject.ts.
#
# Usage (from canary-runtime.sh):
#   limactl shell agent-core-hcr -- \
#     bash /path/to/shadow-supervisor.sh <variant> <shadow.env> <shadow_root> <run_id>
#
# Variants: fresh | dirty
# =============================================================================

set -euo pipefail

VARIANT="${1:-fresh}"
SHADOW_ENV="${2}"
SHADOW_ROOT="${3}"
RUN_ID="${4}"

# Validate args
if [ ! -f "$SHADOW_ENV" ]; then
    echo "FATAL: shadow.env not found at $SHADOW_ENV"
    exit 1
fi

# Binary paths (same as canary-runtime.sh)
KERNEL_BIN="/Users/yanfenma/.agent-core/hcr-linux/agent-core-linux/target/release/agent-core-kernel"
CODING_HARNESS_BIN="/Users/yanfenma/.agent-core/hcr-linux/agent-core-linux/tools/coding-harness/target/release/coding-harness"
DEPLOYMENT_HARNESS_BIN="/Users/yanfenma/.agent-core/hcr-linux/agent-core-linux/tools/deployment-harness/target/release/deployment-harness"
CAPABILITY_HOST_BIN="/Users/yanfenma/.agent-core/hcr-linux/agent-core-linux/tools/capability-host/target/release/capability-host"

# Shadow tool paths (deployed to shared mount by canary-runtime)
SHADOW_TOOLS_DIR="/Users/yanfenma/.agent-core/hcr-linux/shadow-tools"
CONNECTOR_SCRIPT="${SHADOW_TOOLS_DIR}/tools/shadow-canary/connector-shadow.ts"
INJECT_SCRIPT="${SHADOW_TOOLS_DIR}/tools/shadow-canary/inject.ts"
CARD_DIR="${SHADOW_ROOT}/evidence"
LOG_DIR="${SHADOW_ROOT}/logs"
PID_DIR="${SHADOW_ROOT}/pids"
STATE_DIR="${SHADOW_ROOT}/state"
FAILURE_PROXY_BIN="${SHADOW_TOOLS_DIR}/shadow-failure-proxy/target/release/shadow-failure-proxy"

# Ensure directories
mkdir -p "$LOG_DIR" "$PID_DIR" "$STATE_DIR"

# =============================================================================
# Source shadow.env — all env vars for all services
# =============================================================================
set -a
source "$SHADOW_ENV"
set +a

# =============================================================================
# Process management
# =============================================================================

# Track PIDs in an array for clean shutdown
declare -a SPAWNED_PIDS=()

cleanup() {
    local exit_code=$?
    echo "[supervisor] cleanup called (exit=$exit_code)"
    # Kill all spawned processes in reverse order (dependency-first)
    for pid in "${SPAWNED_PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            # Wait up to 3 seconds for graceful exit
            local waited=0
            while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 3 ]; do
                sleep 1
                waited=$((waited + 1))
            done
            # Force kill if still alive
            kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
        fi
    done
    # Also kill entire process group of this script
    # (catches any orphaned grandchild processes)
    local my_pgid
    my_pgid=$(awk '{print $5}' /proc/self/stat 2>/dev/null || echo "")
    if [ -n "$my_pgid" ] && [ "$my_pgid" != "1" ]; then
        kill -- "-$my_pgid" 2>/dev/null || true
    fi
    echo "[supervisor] cleanup complete (exit=$exit_code)"
    exit "$exit_code"
}

trap cleanup EXIT INT TERM HUP

# =============================================================================
# Service start helper
# =============================================================================

start_service() {
    local name="$1"
    local pid_file="$2"
    local log_file="${LOG_DIR}/${name}.log"
    shift 2
    # Run the command, redirecting output to log file
    "$@" > "$log_file" 2>&1 &
    local pid=$!
    echo "$pid" > "$pid_file"
    SPAWNED_PIDS+=("$pid")
    echo "[supervisor] started $name (PID $pid) log=$log_file"
}

wait_for_port() {
    local port="$1"
    local name="$2"
    local timeout="${3:-30}"
    local waited=0
    while [ "$waited" -lt "$timeout" ]; do
        # Check if process is still alive
        local pid_file="${PID_DIR}/${name}.pid"
        if [ -f "$pid_file" ]; then
            local pid
            pid=$(cat "$pid_file")
            if ! kill -0 "$pid" 2>/dev/null; then
                echo "[supervisor] FAIL: $name process (PID $pid) exited before ready!"
                return 1
            fi
        fi
        
        # Check port — different services use different endpoints
        case "$name" in
            coding-harness)
                if curl -sf --max-time 3 -X POST "http://127.0.0.1:${port}/execute" \
                    -H "Content-Type: application/json" \
                    -d '{"protocol_version":"external-harness-v1","operation":"external.coding_workspace_list","arguments":{"workspace_id":"scratch"}}' >/dev/null 2>&1; then
                    echo "[supervisor] ✅ $name ready (port $port)"
                    return 0
                fi
                ;;
            connector)
                local status
                status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 2 \
                    -X POST "http://127.0.0.1:${port}/v1/execute" \
                    -H "Content-Type: application/json" \
                    -d '{}' 2>/dev/null || echo "000")
                if [ "$status" != "000" ]; then
                    echo "[supervisor] ✅ $name ready (port $port, HTTP $status)"
                    return 0
                fi
                ;;
            *)
                if curl -sf --max-time 2 "http://127.0.0.1:${port}/health" >/dev/null 2>&1; then
                    echo "[supervisor] ✅ $name ready (port $port)"
                    return 0
                fi
                ;;
        esac
        sleep 1
        waited=$((waited + 1))
    done
    echo "[supervisor] FAIL: $name not ready on port $port after ${timeout}s"
    return 1
}

check_process_alive() {
    local name="$1"
    local pid_file="${PID_DIR}/${name}.pid"
    if [ ! -f "$pid_file" ]; then
        return 1
    fi
    local pid
    pid=$(cat "$pid_file")
    if ! kill -0 "$pid" 2>/dev/null; then
        echo "[supervisor] FAIL: $name (PID $pid) has exited!"
        return 1
    fi
    return 0
}

# =============================================================================
# Main flow
# =============================================================================

echo "=========================================="
echo " Shadow Supervisor v1"
echo " VARIANT: ${VARIANT}"
echo " ROOT:    ${SHADOW_ROOT}"
echo " RUN_ID:  ${RUN_ID}"
echo "=========================================="
echo ""

# ---- Step 0: Check port conflicts ----
echo "[supervisor] Checking port conflicts..."
PORT_CONFLICT=false
for port_spec in 4130:kernel 4131:connector 7200:coding-harness 7300:capability-host 7400:deployment-harness; do
    port="${port_spec%%:*}"
    name="${port_spec##*:}"
    # Use /proc/net/tcp to check for listening ports (works on all Linux)
    # Port in /proc/net/tcp is in hex, little-endian
    port_hex=$(printf "%04X" ${port})
    if grep -q "0${port_hex:2:2}${port_hex:0:2}" /proc/net/tcp 2>/dev/null; then
        echo "  ⚠️  Port ${port} (${name}) is already in use"
        PORT_CONFLICT=true
    fi
done
if [ "$PORT_CONFLICT" = "true" ]; then
    echo "[supervisor] ❌ SHADOW_PORT_CONFLICT"
    exit 1
fi
echo "[supervisor] All ports free ✓"

# ---- Step 1: Start services ----
echo "[supervisor] Starting services..."

# 1a. Kernel
start_service "kernel" "${PID_DIR}/kernel.pid" \
    "${KERNEL_BIN}" serve --db "${SHADOW_ROOT}/journal/journal.db"
sleep 2

# 1b. NO connector here — inject.ts imports connector-shadow.ts which starts
#     its own execute server on port 4131. Starting it twice causes EADDRINUSE.

# 1c. Coding Harness
start_service "coding-harness" "${PID_DIR}/coding-harness.pid" \
    "${CODING_HARNESS_BIN}" --listen 127.0.0.1:7200

# 1d. Capability Host
start_service "capability-host" "${PID_DIR}/capability-host.pid" \
    "${CAPABILITY_HOST_BIN}"

# 1e. Deployment Harness (fresh: 7400, dirty: 7401 with proxy on 7400)
if [ "${VARIANT}" = "dirty" ]; then
    # Dirty: harness on 7401, proxy on 7400 (Kernel connects to proxy)
    DEPLOYMENT_HARNESS_LISTEN_ADDR=127.0.0.1:7401 \
    start_service "deployment-harness" "${PID_DIR}/deployment-harness.pid" \
        "${DEPLOYMENT_HARNESS_BIN}"

    # Start failure proxy on port 7400 (Node.js version)
    # SHADOW_FAILURE_COUNT=2: 1st deploy (Phase A) forwarded, 2nd deploy (Phase B) fails,
    # subsequent deploys (Phase C) forwarded (remaining=0).
    SHADOW_FAILURE_COUNT=2 \
    start_service "failure-proxy" "${PID_DIR}/failure-proxy.pid" \
        npx tsx "${SHADOW_TOOLS_DIR}/tools/shadow-canary/failure-proxy.ts"
else
    # Fresh: harness on 7400, no proxy
    start_service "deployment-harness" "${PID_DIR}/deployment-harness.pid" \
        "${DEPLOYMENT_HARNESS_BIN}"
fi

echo "[supervisor] All services launched, waiting for readiness..."

# ---- Step 2: Wait for services to be ready ----
echo ""
echo "[supervisor] Waiting for services to be ready..."
FAILED=false

for svc in kernel coding-harness capability-host deployment-harness${ADDITIONAL_WAIT:-}; do
    case "$svc" in
        kernel) port=4130; timeout=30 ;;
        coding-harness) port=7200; timeout=15 ;;
        capability-host) port=7300; timeout=15 ;;
        deployment-harness)
            if [ "${VARIANT}" = "dirty" ]; then
                port=7401
            else
                port=7400
            fi
            timeout=15
            ;;
        failure-proxy) port=7400; timeout=10 ;;
    esac
    if ! wait_for_port "$port" "$svc" "$timeout"; then
        FAILED=true
    fi
done

if [ "$FAILED" = "true" ]; then
    echo "[supervisor] ❌ Not all services ready — aborting"
    exit 1
fi

# ---- Step 3: Verify all processes still alive ----
echo ""
echo "[supervisor] Verifying process persistence..."
ALL_ALIVE=true
for svc in kernel coding-harness capability-host deployment-harness; do
    if ! check_process_alive "$svc"; then
        ALL_ALIVE=false
    fi
done
# Also check failure-proxy for dirty variant
if [ "${VARIANT}" = "dirty" ]; then
    if ! check_process_alive "failure-proxy"; then
        ALL_ALIVE=false
    fi
fi

if [ "$ALL_ALIVE" = "false" ]; then
    echo "[supervisor] ❌ Process(es) exited during startup — aborting"
    exit 1
fi
echo "[supervisor] All processes persistent ✓"

# ---- Step 4: Run inject.ts ----
echo ""
echo "[supervisor] Running inject.ts (${VARIANT})..."
echo ""

INJECT_EXIT_CODE=0
cd "${SHADOW_TOOLS_DIR}/tools/shadow-canary"
npx tsx "${INJECT_SCRIPT}" "${VARIANT}" || INJECT_EXIT_CODE=$?

echo ""
echo "[supervisor] inject.ts finished with exit code ${INJECT_EXIT_CODE}"

# ---- Step 5: Exit with inject's exit code ----
# cleanup() trap will handle stopping all services
exit "$INJECT_EXIT_CODE"
