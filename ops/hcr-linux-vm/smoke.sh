#!/bin/bash
set -euo pipefail

# Run HCR smoke tests against the Linux VM Coding Harness.
# Usage: ./smoke.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"
HOST_PORT="${HCR_HOST_PORT:-7200}"
HCR_TOKEN="${HCR_TOKEN:-dev-token}"

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

echo "=== HCR Smoke Tests ==="

# Test 1: Health endpoint
echo "--- Health ---"
HEALTH=$(curl -sf "$BASE/health" 2>&1 || echo "FAILED")
check "health endpoint" "ok" "$HEALTH"

# Test 2: HCR exec with read_file (cat workspace file)
echo "--- HCR exec: read workspace file ---"
limactl shell "$VM_NAME" -- bash -c '
    mkdir -p /srv/agent-core-hcr/workspaces/test-ws
    echo "hello from hcr" > /srv/agent-core-hcr/workspaces/test-ws/hcr-test.txt
    chmod 644 /srv/agent-core-hcr/workspaces/test-ws/hcr-test.txt
'

RESP=$(curl -sf -X POST "$BASE/exec" \
    -H "Content-Type: application/json" \
    -d "{
        \"workspace_id\": \"default\",
        \"operation\": \"external.coding_hcr_exec\",
        \"args\": {
            \"hcr_profile_id\": \"hcr-v0\",
            \"hcr_token\": \"$HCR_TOKEN\",
            \"command\": \"read_file\",
            \"params\": {
                \"path\": \"/srv/agent-core-hcr/workspaces/test-ws/hcr-test.txt\"
            }
        }
    }" 2>&1 || echo "FAILED")

echo "Response: $RESP"
check "HCR exec returns structured result" '"ok"' "$RESP"
check "HCR exec succeeds" '"status":"succeeded"' "$RESP"
check "HCR exec has exit_code" '"exit_code":0' "$RESP"
check "HCR exec reads content" 'hello from hcr' "$RESP"

# Test 3: HCR exec with invalid token (should fail)
echo "--- HCR exec: invalid token ---"
RESP2=$(curl -sf -X POST "$BASE/exec" \
    -H "Content-Type: application/json" \
    -d "{
        \"workspace_id\": \"default\",
        \"operation\": \"external.coding_hcr_exec\",
        \"args\": {
            \"hcr_profile_id\": \"hcr-v0\",
            \"hcr_token\": \"wrong-token\",
            \"command\": \"read_file\",
            \"params\": {\"path\": \"/tmp/test.txt\"}
        }
    }" 2>&1 || echo "FAILED")

check "HCR exec with wrong token denied" '"ok":false' "$RESP2"
check "HCR exec token error" 'hcr_token' "$RESP2"

# Test 4: Read outside workspace (should fail due to sandbox)
echo "--- HCR exec: read outside workspace ---"
RESP3=$(curl -sf -X POST "$BASE/exec" \
    -H "Content-Type: application/json" \
    -d "{
        \"workspace_id\": \"default\",
        \"operation\": \"external.coding_hcr_exec\",
        \"args\": {
            \"hcr_profile_id\": \"hcr-v0\",
            \"hcr_token\": \"$HCR_TOKEN\",
            \"command\": \"read_file\",
            \"params\": {
                \"path\": \"/etc/passwd\"
            }
        }
    }" 2>&1 || echo "FAILED")

check "HCR exec outside workspace sandbox" '"ok"' "$RESP3"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -gt 0 ]; then exit 1; fi
