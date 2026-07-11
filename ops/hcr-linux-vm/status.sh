#!/bin/bash
set -euo pipefail

# Check the status of the Agent Core HCR environment.
# Usage: ./status.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$SCRIPT_DIR/lib.sh"

# Resolve a native (non-Rosetta) limactl. Aborts with
# LIMA_NATIVE_ARM64_REQUIRED if only an x86_64 limactl is available.
LIMACTL="$(hcr_resolve_limactl)"

VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"
HOST_PORT="${HCR_HOST_PORT:-7200}"
# Workspace used for the health probe (registered by start.sh).
HEALTH_WS="${HCR_HEALTH_WS:-default}"

echo "=== Lima VM: $VM_NAME (limactl: $LIMACTL) ==="
if "$LIMACTL" list "$VM_NAME" --format '{{.Status}}' 2>/dev/null | grep -q Running; then
    echo "Status: Running"
    echo "SSH Port: $("$LIMACTL" list "$VM_NAME" --format '{{.SSHLocalPort}}' 2>/dev/null || echo 'N/A')"

    echo ""
    echo "=== Coding Harness Health ==="
    # The harness speaks the external-harness-v1 JSON protocol on every
    # request (no bare GET /health), so probe with a workspace.list call.
    if curl -sf --max-time 5 -X POST -H "Content-Type: application/json" \
        -d '{"protocol_version":"external-harness-v1","operation":"external.coding_workspace_list","arguments":{"workspace_id":"'"$HEALTH_WS"'"}}' \
        http://127.0.0.1:$HOST_PORT/execute >/dev/null 2>&1; then
        echo "Health: OK (http://127.0.0.1:$HOST_PORT)"
    else
        echo "Health: NOT RESPONDING"
    fi

    echo ""
    echo "=== System Info ==="
    "$LIMACTL" shell "$VM_NAME" -- uname -a 2>/dev/null || echo "(unavailable)"

    echo ""
    echo "=== bubblewrap ==="
    "$LIMACTL" shell "$VM_NAME" -- bwrap --version 2>/dev/null || echo "(unavailable)"

    echo ""
    echo "=== HCR Canary ==="
    "$LIMACTL" shell "$VM_NAME" -- ls /srv/agent-core-hcr/ 2>/dev/null || echo "(runtime dirs not created)"
else
    echo "Status: Stopped"
    exit 1
fi
