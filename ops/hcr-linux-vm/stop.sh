#!/bin/bash
set -euo pipefail

# Stop the Agent Core HCR Linux VM.
# Usage: ./stop.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"

echo "=== Stopping Coding Harness (if running) ==="
limactl shell "$VM_NAME" -- bash -c '
    if [ -f /tmp/coding-harness.pid ]; then
        kill $(cat /tmp/coding-harness.pid) 2>/dev/null || true
        rm -f /tmp/coding-harness.pid
        echo "Coding Harness stopped."
    else
        echo "No Coding Harness PID file found."
    fi
' 2>/dev/null || true

echo "=== Stopping Lima VM: $VM_NAME ==="
limactl stop "$VM_NAME" 2>/dev/null || echo "VM not running."
echo "Done."
