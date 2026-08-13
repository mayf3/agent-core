#!/usr/bin/env bash
# Register the five path-based operations, then remove the legacy workspace
# operations from the active snapshot. Immutable snapshots/manifests remain.
set -euo pipefail

: "${SOURCE_ROOT:?SOURCE_ROOT must be the absolute authoritative source root}"
: "${KERNEL_URL:?KERNEL_URL is required}"
: "${IPC_TOKEN:?IPC_TOKEN is required}"
: "${DB_PATH:?DB_PATH is required}"
: "${EXECUTION_MANIFEST_BUILDER:?EXECUTION_MANIFEST_BUILDER is required}"
: "${CODING_MANIFEST_BUILDER:?CODING_MANIFEST_BUILDER is required}"
: "${EXECUTION_ARTIFACT_DIGEST:?EXECUTION_ARTIFACT_DIGEST is required}"
: "${CODING_ARTIFACT_DIGEST:?CODING_ARTIFACT_DIGEST is required}"

case "$SOURCE_ROOT" in
  /*) ;;
  *) echo "SOURCE_ROOT must be absolute" >&2; exit 2 ;;
esac
test -d "$SOURCE_ROOT"
test -x "$EXECUTION_MANIFEST_BUILDER"
test -x "$CODING_MANIFEST_BUILDER"
test -f "$DB_PATH"

EXECUTION_ENDPOINT="${EXECUTION_ENDPOINT:-http://127.0.0.1:7650/execute}"
CODING_ENDPOINT="${CODING_ENDPOINT:-http://127.0.0.1:7200/execute}"
MIGRATION_TMP="$(mktemp -d)"
trap 'rm -rf "$MIGRATION_TMP"' EXIT

"$EXECUTION_MANIFEST_BUILDER" "$SOURCE_ROOT" \
  --endpoint "$EXECUTION_ENDPOINT" \
  --artifact-digest "$EXECUTION_ARTIFACT_DIGEST" \
  > "$MIGRATION_TMP/execution.jsonl"

"$CODING_MANIFEST_BUILDER" legacy-internal \
  --endpoint "$CODING_ENDPOINT" \
  --artifact-digest "$CODING_ARTIFACT_DIGEST" \
  > "$MIGRATION_TMP/coding.jsonl"

python3 - "$KERNEL_URL" "$IPC_TOKEN" "$DB_PATH" \
  "$MIGRATION_TMP/execution.jsonl" "$MIGRATION_TMP/coding.jsonl" <<'PY'
import json
import sqlite3
import subprocess
import sys

kernel_url, token, db_path, execution_path, coding_path = sys.argv[1:]
headers = ["-H", "Content-Type: application/json", "-H", f"Authorization: Bearer {token}"]


def decode_stream(path):
    raw = open(path, encoding="utf-8").read()
    decoder = json.JSONDecoder()
    values = []
    while raw.strip():
        raw = raw.lstrip()
        value, end = decoder.raw_decode(raw)
        values.append(value)
        raw = raw[end:]
    return values


def request(path, body):
    completed = subprocess.run(
        ["curl", "--fail-with-body", "--silent", "--show-error", "-X", "POST",
         *headers, "--data", json.dumps(body, ensure_ascii=False), f"{kernel_url}{path}"],
        check=True, capture_output=True, text=True,
    )
    response = json.loads(completed.stdout)
    if not response.get("ok"):
        raise RuntimeError(f"{path} failed: {response}")
    return response


connection = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)


def active_snapshot():
    row = connection.execute(
        "SELECT active_snapshot_id FROM registry_state WHERE singleton_id=1"
    ).fetchone()
    if not row:
        raise RuntimeError("active registry snapshot missing")
    return row[0]


manifests = decode_stream(execution_path)
manifests += [
    manifest for manifest in decode_stream(coding_path)
    if manifest["operation_name"] == "external.execution_task"
]
expected = {
    "external.execution_list", "external.execution_read", "external.execution_write",
    "external.execution_exec", "external.execution_task",
}
if {manifest["operation_name"] for manifest in manifests} != expected:
    raise RuntimeError("path manifest set mismatch")

for manifest in manifests:
    request("/v1/harness/register", manifest)
    response = request("/v1/harness/enable", {
        "manifest_id": manifest["manifest_id"],
        "expected_snapshot_id": active_snapshot(),
    })
    print(f"enabled {manifest['operation_name']} -> {response['active_snapshot_id']}")

legacy = [
    "external.coding_workspace_list", "external.coding_workspace_read",
    "external.coding_workspace_write", "external.coding_workspace_exec",
    "external.coding_workspace_task",
]
for operation in legacy:
    row = connection.execute(
        "SELECT binding_key FROM registry_snapshot_operations "
        "WHERE snapshot_id=? AND operation_name=?",
        (active_snapshot(), operation),
    ).fetchone()
    if not row:
        continue
    response = request("/v1/harness/disable", {
        "manifest_id": row[0],
        "expected_snapshot_id": active_snapshot(),
    })
    print(f"disabled {operation} -> {response['active_snapshot_id']}")

remaining = {
    row[0] for row in connection.execute(
        "SELECT operation_name FROM registry_snapshot_operations WHERE snapshot_id=?",
        (active_snapshot(),),
    )
}
if not expected.issubset(remaining):
    raise RuntimeError("active snapshot verification failed")
if set(legacy).intersection(remaining):
    raise RuntimeError("legacy workspace operation remains active")
print(f"active_snapshot={active_snapshot()}")
PY
