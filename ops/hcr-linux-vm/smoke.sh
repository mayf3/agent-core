#!/bin/bash
set -euo pipefail

# Run HCR smoke tests against the Linux VM Coding Harness.
#
# The harness speaks the external-harness-v1 JSON protocol on port $HOST_PORT:
# every request is a POST with a JSON body of the form
#   {"protocol_version":"external-harness-v1",
#    "operation":"<op>",
#    "arguments":{"workspace_id":"<ws>", ...}}
# There is no bare GET /health.
#
# Usage: ./smoke.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$SCRIPT_DIR/lib.sh"

# Resolve a native (non-Rosetta) limactl. Aborts with
# LIMA_NATIVE_ARM64_REQUIRED if only an x86_64 limactl is available.
LIMACTL="$(hcr_resolve_limactl)"

VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"
HOST_PORT="${HCR_HOST_PORT:-7200}"
HCR_TOKEN="${HCR_TOKEN:-dev-token}"
# Workspace registered by start.sh (must match the HCR profile's workspace_id).
WS_ID="${HCR_WS_ID:-default}"
WS_ROOT="${HCR_WS_ROOT:-/srv/agent-core-hcr/workspaces/$WS_ID}"

BASE="http://127.0.0.1:$HOST_PORT"
PASS=0
FAIL=0

check() {
    local name="$1" expected="$2" actual="$3"
    if echo "$actual" | grep -q "$expected"; then
        echo "  ✅ $name"
        PASS=$((PASS + 1))
    else
        echo "  ❌ $name (expected: $expected, got: $actual)"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== HCR Smoke Tests (limactl: $LIMACTL) ==="

# Test 1: Liveness probe via workspace.list (the protocol has no GET /health).
echo "--- Health (workspace.list) ---"
HEALTH=$(curl -sf --max-time 5 -X POST "$BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{\"protocol_version\":\"external-harness-v1\",\"operation\":\"external.coding_workspace_list\",\"arguments\":{\"workspace_id\":\"$WS_ID\"}}" 2>&1 || echo "FAILED")
check "harness reachable + default workspace registered" '"ok":true' "$HEALTH"

# Test 2: HCR exec with read_file (cat a workspace file through bubblewrap).
echo "--- HCR exec: read workspace file ---"
"$LIMACTL" shell "$VM_NAME" -- bash -c "
    mkdir -p '$WS_ROOT'
    echo 'hello from hcr' > '$WS_ROOT/hcr-test.txt'
    chmod 644 '$WS_ROOT/hcr-test.txt'
"

RESP=$(curl -sf --max-time 30 -X POST "$BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{\"protocol_version\":\"external-harness-v1\",\"operation\":\"external.coding_hcr_exec\",\"arguments\":{\"workspace_id\":\"$WS_ID\",\"hcr_profile_id\":\"hcr-v0\",\"hcr_token\":\"$HCR_TOKEN\",\"command\":\"read_file\",\"params\":{\"path\":\"$WS_ROOT/hcr-test.txt\"}}}" 2>&1 || echo "FAILED")

echo "Response: $RESP"
check "HCR exec returns structured result" '"ok":true' "$RESP"
check "HCR exec succeeds" '"status":"succeeded"' "$RESP"
check "HCR exec exit_code 0" '"exit_code":0' "$RESP"
check "HCR exec timed_out false" '"timed_out":false' "$RESP"
check "HCR exec child_cleanup confirmed" '"child_cleanup":"confirmed"' "$RESP"
check "HCR exec reads content" 'hello from hcr' "$RESP"

# Test 3: HCR exec with an invalid token (must be denied).
echo "--- HCR exec: invalid token ---"
RESP2=$(curl -sf --max-time 10 -X POST "$BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{\"protocol_version\":\"external-harness-v1\",\"operation\":\"external.coding_hcr_exec\",\"arguments\":{\"workspace_id\":\"$WS_ID\",\"hcr_profile_id\":\"hcr-v0\",\"hcr_token\":\"wrong-token\",\"command\":\"read_file\",\"params\":{\"path\":\"$WS_ROOT/hcr-test.txt\"}}}" 2>&1 || echo "FAILED")

check "HCR exec with wrong token denied" '"ok":false' "$RESP2"
check "HCR exec token error code" 'hcr_token' "$RESP2"

# Test 4: Read the real user home, which is NOT bind-mounted into the
# bubblewrap sandbox (the sandbox only exposes the workspace, the sandbox
# home, and read-only system paths like /usr, /lib, /etc, /bin). The cat
# child must therefore be unable to read a file under the real home.
echo "--- HCR exec: read real user home (blocked by sandbox) ---"
REAL_HOME_FILE="/home/yanfenma.guest/.ssh/authorized_keys"
RESP3=$(curl -sf --max-time 30 -X POST "$BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{\"protocol_version\":\"external-harness-v1\",\"operation\":\"external.coding_hcr_exec\",\"arguments\":{\"workspace_id\":\"$WS_ID\",\"hcr_profile_id\":\"hcr-v0\",\"hcr_token\":\"$HCR_TOKEN\",\"command\":\"read_file\",\"params\":{\"path\":\"$REAL_HOME_FILE\"}}}" 2>&1 || echo "FAILED")

echo "Response: $RESP3"
# The sandboxed cat cannot see the real home, so the exec must fail
# (non-zero exit code) rather than leak home-directory contents.
check "HCR exec real home blocked (exit_code != 0)" '"exit_code":1' "$RESP3"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -gt 0 ]; then exit 1; fi
