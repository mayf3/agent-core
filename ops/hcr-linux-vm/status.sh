#!/bin/bash
set -euo pipefail

# Check the status of the Agent Core HCR environment.
# Usage: ./status.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"
HOST_PORT="${HCR_HOST_PORT:-7200}"

echo "=== Lima VM: $VM_NAME ==="
if limactl list "$VM_NAME" --format '{{.Status}}' 2>/dev/null | grep -q Running; then
    echo "Status: Running"
    echo "SSH Port: $(limactl list "$VM_NAME" --format '{{.SSHLocalPort}}' 2>/dev/null || echo 'N/A')"

    echo ""
    echo "=== Coding Harness Health ==="
    if curl -sf http://127.0.0.1:$HOST_PORT/health >/dev/null 2>&1; then
        echo "Health: OK (http://127.0.0.1:$HOST_PORT)"
    else
        echo "Health: NOT RESPONDING"
    fi

    echo ""
    echo "=== System Info ==="
    limactl shell "$VM_NAME" -- uname -a 2>/dev/null || echo "(unavailable)"

    echo ""
    echo "=== bubblewrap ==="
    limactl shell "$VM_NAME" -- bwrap --version 2>/dev/null || echo "(unavailable)"

    echo ""
    echo "=== HCR Canary ==="
    limactl shell "$VM_NAME" -- ls /srv/agent-core-hcr/ 2>/dev/null || echo "(runtime dirs not created)"
else
    echo "Status: Stopped"
    exit 1
fi
