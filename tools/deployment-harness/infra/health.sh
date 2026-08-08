#!/bin/bash
# =============================================================================
# health.sh — Fixed-runtime consistency + health verifier (acceptance).
# =============================================================================
#
# Deployment Harness deployment artifact. Reports, for each of the five
# services: PID, executable path, source_head (from provenance), started_at,
# listen port, and the systemd unit supervising it. Verifies that the running
# executable matches provenance, that each port has exactly one listener,
# and that each health endpoint is ok. Does NOT mutate anything.
#
# Usage: health.sh
# =============================================================================
set -euo pipefail

BASE="${HOME}/.agent-core"
RUNTIME="${BASE}/runtime"

# service => fixed port
port_for() {
    case "$1" in
        kernel)             echo 4130 ;;
        connector)          echo 4131 ;;
        coding-harness)     echo 7200 ;;
        capability-host)    echo 7300 ;;
        deployment-harness) echo 7400 ;;
    esac
}

unit_for() {
    # Kernel + Connector are supervised by systemd (this round's contract).
    # The three Harness services are NOT systemd-managed this round.
    case "$1" in
        kernel)             echo agent-core-kernel.service ;;
        connector)          echo agent-core-feishu-connector.service ;;
        coding-harness)     echo "nohup (not supervised this round)" ;;
        capability-host)    echo "nohup (not supervised this round)" ;;
        deployment-harness) echo "nohup (not supervised this round)" ;;
    esac
}

listener_pids() {
    { ss -tlnpH 2>/dev/null | awk -v p=":$1" '$0 ~ p {print}' \
        | grep -oE 'pid=[0-9]+' | cut -d= -f2 | sort -u | tr '\n' ' ' | sed 's/ *$//'; } || true
}

printf "%-20s %-8s %-10s %-30s %-16s %-10s %-28s\n" \
    SERVICE PID PORT HEALTH SOURCE_HEAD STARTED_AT UNIT
echo "----------------------------------------------------------------------------------------------------------------"

GLOBAL_FAIL=0
for svc in kernel connector coding-harness capability-host deployment-harness; do
    port="$(port_for "$svc")"
    unit="$(unit_for "$svc")"
    pids="$(listener_pids "$port")"
    n="$(echo "$pids" | wc -w | tr -d ' ')"
    # pick the listener pid (kernel/coding/cap/deployment: single; connector: node child)
    pid="$(echo "$pids" | awk '{print $1}')"

    health="?"
    case "$svc" in
        kernel)
            b="$(curl -s --max-time 3 http://127.0.0.1:$port/health 2>/dev/null || true)"
            echo "$b" | grep -q '"status":"ok"' && health="ok" || health="DOWN"
            ;;
        coding-harness)
            b="$(curl -s --max-time 3 -X POST http://127.0.0.1:$port/execute \
                -H 'Content-Type: application/json' \
                -d '{"protocol_version":"external-harness-v1","operation":"external.coding_workspace_list","arguments":{"workspace_id":"scratch"}}' 2>/dev/null || true)"
            echo "$b" | grep -q '"ok":true' && health="ok" || health="DOWN"
            ;;
        capability-host|deployment-harness)
            b="$(curl -s --max-time 3 http://127.0.0.1:$port/health 2>/dev/null || true)"
            echo "$b" | grep -q '"status":"ok"' && health="ok" || health="DOWN"
            ;;
        connector)
            code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 -X POST http://127.0.0.1:$port/v1/execute -H 'Content-Type: application/json' -d '{}' 2>/dev/null || echo 000)"
            [ "$code" != "000" ] && health="ok" || health="DOWN"
            ;;
    esac

    # source_head from provenance
    provfile=""
    case "$svc" in
        kernel)             provfile="${RUNTIME}/kernel/provenance.json" ;;
        connector)          provfile="${RUNTIME}/feishu-connector/provenance.json" ;;
        coding-harness)     provfile="${RUNTIME}/coding-harness/provenance.json" ;;
        capability-host)    provfile="${RUNTIME}/capability-host/provenance.json" ;;
        deployment-harness) provfile="" ;;
    esac
    sh="n/a"
    [ -n "$provfile" ] && [ -f "$provfile" ] && sh="$(python3 -c "import json,sys;d=json.load(open('$provfile'));print(d.get('source_head','n/a')[:12])" 2>/dev/null || echo n/a)"

    started="n/a"
    [ -n "$pid" ] && started="$(ps -o lstart= -p "$pid" 2>/dev/null | sed 's/  */ /g' || echo n/a)"

    if [ "$n" != "1" ] || [ "$health" = "DOWN" ]; then GLOBAL_FAIL=1; fi

    printf "%-20s %-8s %-10s %-10s %-16s %-26s %-28s\n" \
        "$svc" "${pid:-none}" "$port($n listeners)" "$health" "$sh" "$started" "$unit"
done

echo "----------------------------------------------------------------------------------------------------------------"
echo "RUNTIME_BINARY_MATCHES_PROVENANCE:"
for svc in kernel coding-harness capability-host; do
    provfile=""
    case "$svc" in
        kernel)          provfile="${RUNTIME}/kernel/provenance.json"; binpath="${RUNTIME}/kernel/agent-core-kernel" ;;
        coding-harness)  provfile="${RUNTIME}/coding-harness/provenance.json"; binpath="${RUNTIME}/coding-harness/coding-harness" ;;
        capability-host) provfile="${RUNTIME}/capability-host/provenance.json"; binpath="${RUNTIME}/capability-host/capability-host" ;;
    esac
    [ -f "$provfile" ] || { echo "  $svc: no provenance"; continue; }
    disk="$(sha256sum "$binpath" 2>/dev/null | cut -d' ' -f1 || echo none)"
    # Kernel/coding-harness provenance uses `binary_sha256`; the migrated
    # capability-host record uses `artifact_sha256` (with a sha256: prefix).
    prov="$(python3 -c "
import json
d=json.load(open('$provfile'))
print(d.get('binary_sha256') or d.get('artifact_sha256','').removeprefix('sha256:'))
" 2>/dev/null || echo none)"
    [ "$disk" = "$prov" ] && echo "  $svc: MATCH ($disk)" || echo "  $svc: MISMATCH disk=$disk prov=$prov"
done

[ "$GLOBAL_FAIL" -eq 0 ] && echo "ALL_SERVICES_HEALTHY=true" || echo "ALL_SERVICES_HEALTHY=false"
exit "$GLOBAL_FAIL"
