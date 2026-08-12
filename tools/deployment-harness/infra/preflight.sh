#!/bin/bash
# =============================================================================
# preflight.sh — ExecStartPre guard for each fixed-runtime service unit.
# =============================================================================
#
# Deployment Harness deployment artifact. Runs before systemd starts a service.
#
# It enforces two things and NOTHING else:
#
#   1. Port hygiene. If the service's fixed loopback port is occupied, the
#      occupant is only removed when its /proc/<pid>/exe matches THIS service's
#      binary (or, for the connector, node + the feishu connector tree). A
#      foreign owner is never killed — the unit fails closed so an operator
#      sees the conflict instead of an accidental kill. After any termination
#      it waits for the port to actually be released before returning.
#
#   2. Kernel-readiness gate (connector only). The connector is never started
#      until the Kernel's HTTP service is reachable and its /health reports
#      ok=true with status "ok" OR "degraded" (degraded = the Kernel has
#      permanent state unknowns, e.g. a terminal-unknown outbox row, while its
#      core APIs — ingress / chat-control / events / outbox delivery — remain
#      serviceable, which is all the Connector depends on). Fail closed on
#      unreachable / malformed / corrupt (ok=false) / unrecognized status, so
#      a message can never be received with no Kernel behind it.
#
# systemd owns single-instance and restart; this script owns only the
# pre-start cleanup + the readiness ordering gate.
#
# Usage: preflight.sh <service>
# =============================================================================
set -euo pipefail

BASE="${HOME}/.agent-core"
RUNTIME="${BASE}/runtime"

port_for() {
    case "$1" in
        kernel)             echo 4130 ;;
        connector)          echo 4131 ;;
        coding-harness)     echo 7200 ;;
        capability-host)    echo 7300 ;;
        deployment-harness) echo 7400 ;;
        *) echo ""; return 1 ;;
    esac
}

binary_for() {
    case "$1" in
        kernel)             echo "${RUNTIME}/kernel/agent-core-kernel" ;;
        coding-harness)     echo "${RUNTIME}/coding-harness/coding-harness" ;;
        capability-host)    echo "${RUNTIME}/capability-host/capability-host" ;;
        deployment-harness) find "${BASE}/deployment/artifacts/deployment-harness" \
                                -path "*/bin/deployment-harness" -type f -executable 2>/dev/null \
                              | xargs -r ls -1t 2>/dev/null | head -1 ;;
        connector)          echo "/usr/local/bin/node" ;;
    esac
}

# All PIDs currently listening on a given loopback port (space separated).
# Guarded with `|| true` so an empty result does not trip `set -o pipefail`.
port_pids() {
    local port="$1"
    { ss -tlnpH 2>/dev/null \
        | awk -v p=":$port " '$0 ~ p {print}' \
        | grep -oE 'pid=[0-9]+' | cut -d= -f2 \
        | sort -u | tr '\n' ' ' | sed 's/ *$//'; } || true
}

# Does this PID's executable match the expected binary?
exe_matches() {
    local pid="$1" expected="$2"
    [ -n "$pid" ] || return 1
    [ -n "$expected" ] || return 1
    [ -r "/proc/$pid/exe" ] || return 1
    local observed expected_resolved
    observed="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
    expected_resolved="$(readlink -f "$expected" 2>/dev/null || echo "$expected")"
    [ "$observed" = "$expected_resolved" ]
}

# Connector's listening PID is a node child; the process tree root is a tsx
# parent. Match if the PID runs node AND its cmdline references the feishu
# connector tree.
connector_matches() {
    local pid="$1"
    [ -n "$pid" ] || return 1
    [ -r "/proc/$pid/exe" ] || return 1
    local observed
    observed="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
    [ "$observed" = "/usr/local/bin/node" ] || return 1
    tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q "connectors/feishu"
}

# Wait until the loopback port has no listener, up to a deadline.
wait_port_free() {
    local port="$1" deadline waited=0
    deadline="${2:-15}"
    while [ "$waited" -lt "$deadline" ]; do
        [ -z "$(port_pids "$port")" ] && return 0
        sleep 1
        waited=$((waited + 1))
    done
    return 1
}

matches_for() {
    local svc="$1" pid="$2" bin
    bin="$(binary_for "$svc")"
    if [ "$svc" = "connector" ]; then
        connector_matches "$pid" || exe_matches "$pid" "$bin"
    else
        exe_matches "$pid" "$bin"
    fi
}

# ---- 2. Kernel-readiness gate (connector only) ----
#
# The Kernel reports status:"degraded" when its state contains permanent
# unknowns (e.g. a terminal-unknown outbox row) — its core HTTP APIs
# (ingress / chat-control / events / outbox delivery) remain serviceable,
# which is everything the Connector depends on. "ok" AND "degraded" both
# pass; anything else (unreachable, malformed, corrupt/ok=false, or an
# unrecognized status) fails closed.
#
# `attempts` bounds the wait loop (production default 120 ≈ systemd's start
# timeout; tests pass a small value to fail fast).
kernel_health_ready() {
    local url="${1:-http://127.0.0.1:4130/health}"
    local attempts="${2:-120}"
    local i=0
    while [ "$i" -lt "$attempts" ]; do
        local body verdict
        body="$(curl -s --max-time 2 "$url" 2>/dev/null || true)"
        verdict=""
        if [ -n "$body" ]; then
            verdict="$(printf '%s' "$body" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(2)          # malformed body — fail closed
if d.get("ok") is not True:
    sys.exit(1)          # corrupt / not ok — fail closed
status = d.get("status")
if status not in ("ok", "degraded"):
    sys.exit(1)          # unrecognized status — fail closed
print(status)
' 2>/dev/null)" || verdict=""
        fi
        if [ -n "$verdict" ]; then
            echo "preflight[connector]: Kernel healthy (status=$verdict) — proceeding"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    echo "preflight[connector]: Kernel not ready after ${attempts}s — refusing to start connector" >&2
    return 1
}

main() {
    local SVC="${1:?usage: preflight.sh <service>}"

# ---- 1. Port hygiene ----
PORT="$(port_for "$SVC")"
if [ -n "$PORT" ]; then
    OCCUPANTS="$(port_pids "$PORT")"
    if [ -n "$OCCUPANTS" ]; then
        echo "preflight[$SVC]: port $PORT occupied by: $OCCUPANTS"
        for pid in $OCCUPANTS; do
            if matches_for "$SVC" "$pid"; then
                # A stale/leftover instance of THIS service — terminate it.
                echo "preflight[$SVC]: terminating stale own instance pid=$pid"
                # Connector: signal the whole node tree (tsx parent + node child).
                if [ "$SVC" = "connector" ]; then
                    root="$pid"
                    # Walk up to the tsx launcher parent for a clean group stop.
                    ppid_p="$(awk '/^PPid:/{print $2}' "/proc/$pid/status" 2>/dev/null || true)"
                    [ -n "$ppid_p" ] && root="$ppid_p"
                    kill -TERM "$root" "$pid" 2>/dev/null || true
                else
                    kill -TERM "$pid" 2>/dev/null || true
                fi
            else
                echo "preflight[$SVC]: port $PORT held by foreign pid=$pid ($(readlink -f "/proc/$pid/exe" 2>/dev/null)) — NOT killing; failing closed" >&2
                exit 1
            fi
        done
        if ! wait_port_free "$PORT" 15; then
            # Force-kill only own matches that survived TERM.
            for pid in $OCCUPANTS; do
                if matches_for "$SVC" "$pid"; then
                    kill -KILL "$pid" 2>/dev/null || true
                fi
            done
            wait_port_free "$PORT" 5 || {
                echo "preflight[$SVC]: port $PORT still occupied after cleanup" >&2
                exit 1
            }
        fi
        echo "preflight[$SVC]: port $PORT released"
    fi
fi


if [ "$SVC" = "connector" ]; then
    kernel_health_ready || return 1
fi

return 0
}

# Run as a script only; allow sourcing for tests of the gate function.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
fi
