#!/bin/bash
set -euo pipefail

# Stop the Agent Core HCR Linux VM.
# Usage: ./stop.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$SCRIPT_DIR/lib.sh"

# Resolve a native (non-Rosetta) limactl. Aborts with
# LIMA_NATIVE_ARM64_REQUIRED if only an x86_64 limactl is available.
LIMACTL="$(hcr_resolve_limactl)"

VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"

echo "=== Stopping Coding Harness (if running) ==="
"$LIMACTL" shell "$VM_NAME" -- bash -c '
    if [ -f /tmp/coding-harness.pid ]; then
        kill $(cat /tmp/coding-harness.pid) 2>/dev/null || true
        rm -f /tmp/coding-harness.pid
        echo "Coding Harness stopped."
    else
        echo "No Coding Harness PID file found."
    fi
' 2>/dev/null || true

echo "=== Stopping Lima VM: $VM_NAME ==="
"$LIMACTL" stop "$VM_NAME" 2>/dev/null || echo "VM not running."
echo "Done."
