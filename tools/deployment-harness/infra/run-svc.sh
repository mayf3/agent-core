#!/bin/bash
# =============================================================================
# run-svc.sh — Per-service launcher for the fixed Agent Core runtime.
# =============================================================================
#
# Deployment Harness deployment artifact. Each of the five fixed-runtime
# services is started through this wrapper by its systemd --user unit.
#
# Responsibilities of this wrapper (intentionally minimal):
#   1. Source the AUTHORITATIVE config at ~/.agent-core/config/runtime.env.
#   2. Rotate the per-service log once per start (never overwrite the only
#      crash evidence).
#   3. exec the exact binary + argv currently in production, with the same
#      working directory. Zero behaviour change vs. the running processes.
#
# It deliberately contains NO orchestration, ordering, restart, or health
# logic — those belong to systemd (single instance + auto-recovery) and to
# preflight.sh (Kernel-readiness gate). Kernel/repository code owns no
# process-hosting responsibility.
#
# Usage: run-svc.sh <kernel|connector|coding-harness|capability-host|deployment-harness>
# =============================================================================
set -euo pipefail

BASE="${HOME}/.agent-core"
CONFIG="${BASE}/config/runtime.env"
RUNTIME="${BASE}/runtime"
DATA="${BASE}/data"
LOGS="${BASE}/logs"
KEEP_LOGS="${AGENT_CORE_LOG_KEEP:-5}"

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <kernel|connector|coding-harness|capability-host|deployment-harness>" >&2
    exit 64
fi
SVC="$1"

if [ ! -f "$CONFIG" ]; then
    echo "FATAL: authoritative config not found: $CONFIG" >&2
    exit 1
fi
# Load TWO authoritative env sources, exactly as the migrated runtime does:
#   1. shared config   (~/.agent-core/config/runtime.env)            — common keys
#   2. per-service env (~/.agent-core/runtime/<svc>/runtime.env)     — service-specific keys
# `set -a` exports every assignment so plain KEY=value files are honoured too.
# Service-specific keys override shared ones (loaded last).
set -a
# shellcheck source=/dev/null
source "$CONFIG"
SVC_ENV="${RUNTIME}/${SVC}/runtime.env"
if [ -f "$SVC_ENV" ]; then
    # shellcheck source=/dev/null
    source "$SVC_ENV"
fi
set +a

mkdir -p "$LOGS"

# ---- log rotation: keep crash evidence, never truncate on start ----
rotate_log() {
    local log="$1"
    if [ -f "$log" ]; then
        local ts
        ts="$(date -u +%Y%m%dT%H%M%SZ)"
        mv "$log" "${log}.${ts}"
        # Keep only the most recent KEEP_LOGS rotated copies.
        ls -1t "${log}."* 2>/dev/null | tail -n +"$((KEEP_LOGS + 1))" | while IFS= read -r old; do
            rm -f "$old"
        done
    fi
}

# Resolve the currently-installed Deployment Harness binary. It lives under a
# content-addressed path (deployment/artifacts/deployment-harness/<sha>/<digest>/bin/).
# Pick the directory that contains the binary the running instance uses, else
# the most recent installation.
resolve_deployment_harness_bin() {
    local root="${BASE}/deployment/artifacts/deployment-harness"
    [ -d "$root" ] || return 1
    local bin
    bin="$(find "$root" -path "*/bin/deployment-harness" -type f -executable 2>/dev/null \
            | xargs -r ls -1t 2>/dev/null | head -1)"
    [ -n "$bin" ] || return 1
    echo "$bin"
}

case "$SVC" in
    kernel)
        rotate_log "${LOGS}/kernel.log"
        exec "${RUNTIME}/kernel/agent-core-kernel" serve \
            --db "${DATA}/agent-core.db"
        ;;
    connector)
        cd "${RUNTIME}/feishu-connector"
        rotate_log "${LOGS}/connector.log"
        exec node "node_modules/tsx/dist/cli.mjs" \
            "connectors/feishu/src/index.ts"
        ;;
    coding-harness)
        cd "${RUNTIME}/coding-harness"
        rotate_log "${LOGS}/coding-harness.log"
        exec "./coding-harness" --listen 127.0.0.1:7200
        ;;
    capability-host)
        cd "${RUNTIME}/capability-host"
        rotate_log "${LOGS}/capability-host.log"
        exec "./capability-host"
        ;;
    deployment-harness)
        DH_BIN="$(resolve_deployment_harness_bin)" || {
            echo "FATAL: deployment-harness binary not found under ${BASE}/deployment/artifacts" >&2
            exit 1
        }
        rotate_log "${LOGS}/deployment-harness.log"
        exec "$DH_BIN"
        ;;
    *)
        echo "usage: $0 <kernel|connector|coding-harness|capability-host|deployment-harness>" >&2
        exit 64
        ;;
esac
