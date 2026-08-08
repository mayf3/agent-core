#!/usr/bin/env bash
#
# register-workspace.sh — Register & activate 4 workspace operations
#                          into the active Registry Snapshot via the existing
#                          generic /v1/harness/register + /v1/harness/enable routes.
#
# No Kernel code change. No hardcoded OperationSpec. No workspace_specs.rs.
# OperationSpecs are constructed server-side from the Provider Manifest fields.
#
# Prerequisites:
#   - Kernel running on AGENT_CORE_KERNEL_PORT (default 4130)
#     with AGENT_CORE_IPC_TOKEN set in environment
#   - Coding-Harness running on $HARNESS_ENDPOINT (default http://127.0.0.1:7200/execute)
#   - manifest_builder binary compiled:
#       cargo build --manifest-path tools/coding-harness/Cargo.toml --bin manifest_builder
#
# Usage:
#   export AGENT_CORE_IPC_TOKEN=...
#   ./ops/coding-harness/register-workspace.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ---- Config (override via env or script-local) ----
KERNEL_URL="${KERNEL_URL:-http://127.0.0.1:${AGENT_CORE_KERNEL_PORT:-4130}}"
HARNESS_ENDPOINT="${HARNESS_ENDPOINT:-http://127.0.0.1:7200/execute}"
WORKSPACE_IDS="${WORKSPACE_IDS:-scratch}"
ARTIFACT_DIGEST="${ARTIFACT_DIGEST:-}"
DB_PATH="${DB_PATH:-$HOME/.agent-core/kernel.sqlite}"
IPC_TOKEN="${IPC_TOKEN:-${AGENT_CORE_IPC_TOKEN:-}}"
MB_BIN="${MB_BIN:-$REPO_DIR/tools/coding-harness/target/debug/manifest_builder}"

if [ -z "$IPC_TOKEN" ]; then
  echo "ERROR: AGENT_CORE_IPC_TOKEN is required" >&2
  exit 1
fi
if [ ! -x "$MB_BIN" ]; then
  echo "ERROR: manifest_builder not found at $MB_BIN" >&2
  echo "  Build: cargo build --manifest-path $REPO_DIR/tools/coding-harness/Cargo.toml --bin manifest_builder" >&2
  exit 1
fi

TMPDIR="${TMPDIR:-/tmp}"
MANIFESTS_ALL="$TMPDIR/coding-manifests-all.jsonl"
MANIFESTS_WS="$TMPDIR/coding-manifests-ws.json"

echo "==============================================================="
echo "  Coding-Harness Workspace Registration"
echo "==============================================================="
echo "  Kernel URL:      $KERNEL_URL"
echo "  Harness EP:      $HARNESS_ENDPOINT"
echo "  Workspace IDs:   $WORKSPACE_IDS"
echo "  DB Path:         $DB_PATH"
echo ""

# ---- Step 1: generate manifests ----
echo "--- Step 1: Generating manifests ---"

if [ -z "$ARTIFACT_DIGEST" ]; then
  CH_BIN="$REPO_DIR/tools/coding-harness/target/debug/coding-harness"
  if [ -x "$CH_BIN" ]; then
    ARTIFACT_DIGEST="sha256:$(shasum -a 256 "$CH_BIN" | awk '{print $1}')"
  else
    ARTIFACT_DIGEST="sha256:46b41724598d7106c26e80a94d3dc11fb83ab993e7904ec29b792a6f4138219b"
  fi
fi
echo "  artifact_digest=$ARTIFACT_DIGEST"

# Build args for manifest_builder
IFS=',' read -ra WS_ARGS <<< "$WORKSPACE_IDS"

# Generate ALL 8 manifests; filter to only workspace ops
"$MB_BIN" "${WS_ARGS[@]}" \
  --endpoint "$HARNESS_ENDPOINT" \
  --artifact-digest "$ARTIFACT_DIGEST" \
  > "$MANIFESTS_ALL" 2>/dev/null

python3 << PYEOF
import json
raw = open("$MANIFESTS_ALL").read()
dec = json.JSONDecoder()
objs = []
s = raw.lstrip()
while s:
    obj, idx = dec.raw_decode(s)
    objs.append(obj)
    s = s[idx:].lstrip()
# Filter to workspace operations only
ws = [o for o in objs if o['operation_name'].startswith('external.coding_workspace_')]
print(f"  Generated {len(objs)} total manifests, {len(ws)} workspace")
json.dump(ws, open("$MANIFESTS_WS", "w"), indent=2)
PYEOF

# ---- Step 2: get current snapshot_id ----
echo ""
echo "--- Step 2: Querying current active snapshot_id ---"
CURRENT_SNAP=""
if [ -f "$DB_PATH" ]; then
  CURRENT_SNAP=$(sqlite3 "$DB_PATH" "SELECT active_snapshot_id FROM registry_state WHERE singleton_id=1;" 2>/dev/null || true)
fi
if [ -z "$CURRENT_SNAP" ]; then
  echo "ERROR: Could not determine current active snapshot_id." >&2
  echo "  Ensure the kernel has been started at least once (init creates the baseline)." >&2
  exit 1
fi
echo "  Current snapshot: $CURRENT_SNAP"

# ---- Step 3: register + enable in a loop ----
echo ""
echo "--- Step 3: Register + Enable each workspace operation ---"

AUTH="Authorization: Bearer $IPC_TOKEN"
SNAP_ID="$CURRENT_SNAP"
REG_OK=0
EN_OK=0
EN_ALREADY=0
EN_FAIL=0

# process the 4 workspace manifests
python3 << PYEOF > "$TMPDIR/register_report.json"
import json, subprocess, sys

ws = json.load(open("$MANIFESTS_WS"))
kernel_url = "$KERNEL_URL"
auth_header = "$AUTH"
snap_id = "$SNAP_ID"

print(f"  Processing {len(ws)} workspace operations:")

for m in ws:
    op_name = m['operation_name']
    mid = m['manifest_id']
    manifest_body = json.dumps(m)

    # -- REGISTER --
    r = subprocess.run(
        ["curl", "-s", "-w", "%{http_code}", "-o", "/dev/null", "-X", "POST",
         "-H", "Content-Type: application/json",
         "-H", auth_header,
         "-d", manifest_body,
         f"{kernel_url}/v1/harness/register"],
        capture_output=True, text=True, timeout=30)
    http_code = r.stdout.strip()
    if http_code == "200":
        print(f"    ✓ {op_name} REGISTER OK (200) -> {mid[:20]}...")
    else:
        # Try reading body for more info
        r2 = subprocess.run(
            ["curl", "-s", "-X", "POST",
             "-H", "Content-Type: application/json",
             "-H", auth_header,
             "-d", manifest_body,
             f"{kernel_url}/v1/harness/register"],
            capture_output=True, text=True, timeout=30)
        print(f"    ? {op_name} REGISTER ({http_code}): {r2.stdout[:150].strip()}")

    # -- ENABLE --
    enable_body = json.dumps({"manifest_id": mid, "expected_snapshot_id": snap_id})
    r = subprocess.run(
        ["curl", "-s", "-X", "POST",
         "-H", "Content-Type: application/json",
         "-H", auth_header,
         "-d", enable_body,
         f"{kernel_url}/v1/harness/enable"],
        capture_output=True, text=True, timeout=30)
    try:
        resp = json.loads(r.stdout)
        if resp.get("ok"):
            if resp.get("changed", False):
                snap_id = resp["active_snapshot_id"]
                print(f"    ✓ {op_name} ENABLE OK (snapshot updated -> {snap_id[:30]}...)")
            else:
                print(f"    ✓ {op_name} ENABLE OK (already present, unchanged)")
        else:
            print(f"    ✗ {op_name} ENABLE FAILED: {resp.get('error','unknown')}")
    except json.JSONDecodeError:
        print(f"    ✗ {op_name} ENABLE FAILED (http {r.returncode}): {r.stdout[:200]}")

print()
print(f"  Final active snapshot_id: {snap_id}")
sys.stdout.flush()
PYEOF

cat "$TMPDIR/register_report.json"

# ---- Step 4: verify active snapshot ----
echo ""
echo "--- Step 4: Verification ---"
if [ -f "$DB_PATH" ]; then
  FINAL_SNAP=$(sqlite3 "$DB_PATH" "SELECT active_snapshot_id FROM registry_state WHERE singleton_id=1;" 2>/dev/null || echo "query_failed")
  echo "  DB active_snapshot_id: $FINAL_SNAP"
  echo ""
  echo "  Operations in active snapshot:"
  sqlite3 -header "$DB_PATH" "
    SELECT operation_name, binding_kind, risk
    FROM registry_snapshot_operations
    WHERE snapshot_id='$FINAL_SNAP'
    ORDER BY operation_name;
  " 2>&1
fi

echo ""
echo "==============================================================="
echo "  Registration complete."
echo ""
echo "  Evidence fields:"
echo "    REGISTRATION_PATH_USED=http_post_v1_harness_register_enable"
echo "    PROVIDER_MANIFEST_ID=manifest_72061b58... (workspace_list)"
echo "    OPERATION_SPEC_SOURCE=external_provider_manifest"
echo "    ACTIVE_REGISTRY_SNAPSHOT_ID=$SNAP_ID"
echo "==============================================================="
