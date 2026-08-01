#!/usr/bin/env bash
#
# acceptance-segmented-jobs.sh — Real-binary acceptance for the persistent
# segmented Development Harness V0.
#
# Proves (with a side-effect-free task that deterministically exceeds the
# single-segment budget):
#   1. submit returns a real job_id quickly (status=accepted, accepted_at,
#      task_digest);
#   2. the submitting Run can end immediately (submit is non-blocking);
#   3. segment 1 exhaustion writes a checkpoint;
#   4. no user "continue" is needed;
#   5. the Harness automatically starts segment 2;
#   6. the Harness continues after a real process restart (kill -9);
#   7. a Completion Receipt is produced;
#   8. "segment 1 ended" is NOT reported as "task complete".
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PORT="${ACCEPTANCE_PORT:-7201}"
BASE="${TMPDIR:-/tmp}/coding-harness-acceptance"
WS_ROOT="$BASE/ws"
ART_ROOT="$BASE/artifacts"
STORE="$BASE/jobs"
HARNESS_LOG="$BASE/harness.log"
CONTROL_TOKEN="acceptance-control-token"
JOB_ID=""

rm -rf "$BASE"
mkdir -p "$WS_ROOT" "$ART_ROOT"

echo "=== Building coding-harness ==="
cargo build --manifest-path "$REPO_DIR/tools/coding-harness/Cargo.toml" --release 2>/dev/null || \
  cargo build --manifest-path "$REPO_DIR/tools/coding-harness/Cargo.toml"
BIN="$REPO_DIR/tools/coding-harness/target/debug/coding-harness"
[ -x "$BIN" ] || BIN="$REPO_DIR/tools/coding-harness/target/release/coding-harness"

export CODING_HARNESS_CONTROL_TOKEN="$CONTROL_TOKEN"
export HARNESS_ARTIFACT_ROOT="$ART_ROOT"
export HARNESS_JOB_STORE="$STORE"
export CODING_CONFIG="{\"workspaces\":{\"acceptance\":{\"root\":\"$WS_ROOT\",\"read\":true,\"write\":true,\"exec\":true,\"opencode\":false,\"network\":false,\"shell\":false,\"segment_budget\":{\"max_model_rounds\":2,\"max_wall_time_ms\":60000,\"max_tool_calls\":10,\"single_tool_timeout_ms\":30000,\"on_exhaustion\":\"checkpoint_and_continue\"}}}}"

start_harness() {
  "$BIN" --listen "127.0.0.1:$PORT" >"$HARNESS_LOG" 2>&1 &
  HARNESS_PID=$!
  for _ in $(seq 1 50); do
    if curl -s -m 1 "http://127.0.0.1:$PORT/execute" -X POST \
        -H "Content-Type: application/json" \
        -d '{"protocol_version":"external-harness-v1","operation":"external.coding_workspace_list","arguments":{"workspace_id":"acceptance"}}' \
        >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.2
  done
  echo "HARNESS FAILED TO START"; tail -20 "$HARNESS_LOG"; exit 1
}

stop_harness() {
  if [ -n "${HARNESS_PID:-}" ]; then kill -9 "$HARNESS_PID" 2>/dev/null || true; wait "$HARNESS_PID" 2>/dev/null || true; fi
}

req() { # operation, args-json
  curl -s -m 10 "http://127.0.0.1:$PORT/execute" -X POST \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $CONTROL_TOKEN" \
    -d "{\"protocol_version\":\"external-harness-v1\",\"operation\":\"$1\",\"arguments\":$2}"
}

status_of() { req external.coding_task_status "{\"task_id\":\"$JOB_ID\"}"; }

echo "=== Start harness (process #1) ==="
start_harness
echo "harness pid=$HARNESS_PID"

echo "=== 1. Submit: must return accepted receipt quickly ==="
T0=$(date +%s)
SUBMIT=$(req external.coding_task_submit "{\"workspace_id\":\"acceptance\",\"objective\":\"fake_work_units:5\",\"acceptance_criteria\":\"fake acceptance\",\"backend\":\"fake\"}")
T1=$(date +%s)
JOB_ID=$(echo "$SUBMIT" | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['job_id'])")
echo "submit round-trip: $((T1-T0))s; job_id=$JOB_ID"
echo "$SUBMIT" | python3 -m json.tool | head -14
echo "$SUBMIT" | grep -q '"status":"accepted"' || { echo "FAIL: not accepted"; exit 1; }
echo "$SUBMIT" | grep -q '"task_digest"' || { echo "FAIL: no task_digest"; exit 1; }

echo "=== 3+4+8. Wait for segment 1 exhaustion (checkpoint, still accepted, no 'continue') ==="
for i in $(seq 1 100); do
  S=$(status_of)
  ST=$(echo "$S" | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['status'])")
  N=$(echo "$S" | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['segment_count'])")
  if [ "$ST" = "accepted" ] && [ "$N" -ge 1 ]; then break; fi
  sleep 0.2
done
echo "after segment 1: status=$ST segments=$N"
[ "$ST" = "accepted" ] || { echo "FAIL: expected accepted after segment 1 (not completed)"; exit 1; }
echo "$S" | python3 -c "
import json,sys
r=json.load(sys.stdin)['result']
cp=r['checkpoint']
assert cp and len(cp['completed_steps'])>0 and len(cp['remaining_steps'])>0, 'checkpoint missing progress'
assert r['segments'][0]['outcome']=='exhausted', 'segment 1 must be exhausted'
assert r['segments'][0]['budget_frozen']['decision_digest'], 'budget decision digest must be frozen'
assert r['segments'][0]['budget_frozen']['hook_id']=='builtin:segment-budget-default-v0'
print('checkpoint completed_steps:', cp['completed_steps'])
print('checkpoint remaining_steps:', cp['remaining_steps'])
print('frozen hook:', r['segments'][0]['budget_frozen']['hook_id'], r['segments'][0]['budget_frozen']['decision_digest'][:16])
print('PASS: checkpoint persisted; segment end != task complete')
"

echo "=== 5. Segment 2 starts AUTOMATICALLY (no user action) ==="
for i in $(seq 1 50); do
  S=$(status_of)
  N=$(echo "$S" | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['segment_count'])")
  AT=$(echo "$S" | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['attempt'])")
  if [ "$N" -ge 2 ]; then break; fi
  sleep 0.2
done
echo "segments=$N attempt=$AT"
[ "$N" -ge 2 ] || { echo "FAIL: segment 2 did not auto-start"; exit 1; }

echo "=== 6. Restart recovery: kill -9 the harness, restart, job continues ==="
stop_harness
echo "killed harness pid=$HARNESS_PID; jobs on disk:"
ls "$STORE"/*.json
start_harness
echo "restarted harness pid=$HARNESS_PID"
for i in $(seq 1 150); do
  S=$(status_of)
  ST=$(echo "$S" | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['status'])")
  N=$(echo "$S" | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['segment_count'])")
  if [ "$ST" = "completed" ] || [ "$ST" = "failed" ]; then break; fi
  sleep 0.2
done
echo "final status=$ST segments=$N"
[ "$ST" = "completed" ] || { echo "FAIL: job did not complete after restart"; exit 1; }

echo "=== 7. Completion receipt ==="
echo "$S" | python3 -c "
import json,sys
r=json.load(sys.stdin)['result']
assert r['status']=='completed', r['status']
assert r['result']['test_result']=='fake: passed'
assert r['checkpoint']['remaining_steps']==[], 'remaining steps must be empty on completion'
assert r['segment_count']>=3, 'multi-segment completion expected'
assert any(s['outcome']=='exhausted' for s in r['segments']), 'exhausted receipts expected'
last=[s for s in r['segments'] if s['outcome']=='completed']
assert last, 'completion segment receipt expected'
print('result_summary:', r['summary'])
print('segments:', [(s['segment_no'], s['outcome']) for s in r['segments']])
print('PASS: Completion Receipt produced after', r['segment_count'], 'segments')
"
ls "$STORE/notifications/" | grep "${JOB_ID}_completed" || { echo "FAIL: no completion notification record"; exit 1; }
echo "notification record present: $(ls "$STORE/notifications/" | grep ${JOB_ID} | tr '\n' ' ')"

stop_harness
echo ""
echo "ALL ACCEPTANCE CHECKS PASSED (job_id=$JOB_ID)"
