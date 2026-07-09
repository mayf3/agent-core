# Context Harness Real Smoke

## 1. What this verifies

This smoke test validates the full end-to-end pipeline:

```
Kernel Runtime
→ context.prepare.v0 HTTP hook
→ tools/context-harness (External Context Harness)
→ ContextFragment
→ LLM prompt context
→ LLM reply reflects the external context
→ Journal contains HookCallRecorded(status=ok)
```

It proves that the Kernel's `HttpHookClient` can call an external HTTP service,
receive a `ContextFragment`, inject it into the LLM context window, and have
the LLM incorporate the fragment content into its response — without leaking
the fragment body or request/response payloads into the Journal's
`HookCallRecorded` event.

## 2. How this differs from unit tests

Existing tests (PR #183, `hook_wiring_tests.rs`) verify that:

- The Runtime calls the hook client when `enabled=true`
- The Journal records `HookCallRecorded`
- Failure modes (`FailOpen`, `FailClosed`) behave correctly
- Missing endpoint URL is handled safely

Those tests use a **fake TCP server** and a **fake/simulated LLM**
(`LocalEchoLlm`). They verify the Kernel-side wiring but **do not** prove that
a real LLM actually reads and responds to the injected fragment content.

This real smoke test uses:

- A **real HTTP server** (`tools/context-harness`)
- A **real LLM endpoint** (OpenAI-compatible API)
- A **real Kernel server** (`agent-core-kernel serve`)
- A **fixed smoke fragment**: `EXTERNAL_CONTEXT_SMOKE_WORD: papaya`

If the LLM reply contains `papaya`, the full pipeline is confirmed working.

## 3. How to start the Context Harness

```bash
# From the repo root:
node tools/context-harness/server.ts

# Or via npm:
npm run context-harness
```

The server listens on `127.0.0.1:17400` by default. Override with:

```bash
PORT=17400 node tools/context-harness/server.ts
```

Verify it is running:

```bash
curl -sS http://127.0.0.1:17400/health
# Expected: {"status":"ok"}
```

Directly test the hook endpoint:

```bash
curl -sS -X POST http://127.0.0.1:17400/context.prepare.v0 \
  -H 'content-type: application/json' \
  -d '{
    "hook":"context.prepare.v0",
    "request_id":"smoke_req_001",
    "timestamp":"2026-07-09T00:00:00Z",
    "payload":{}
  }' | python3 -m json.tool
```

The response `payload.fragments[0].content` must be
`EXTERNAL_CONTEXT_SMOKE_WORD: papaya`.

## 4. Kernel hook environment

Add these to your `.env` or export before starting the Kernel:

```bash
export AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true
export AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0
export AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open
export AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS=3000
```

These env vars are loaded by `KernelConfig::from_cli()`:

| Env | Purpose | Default |
|---|---|---|
| `AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED` | Master switch | `false` |
| `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL` | Harness endpoint URL | `""` |
| `AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE` | Behaviour on failure | `disabled` |
| `AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS` | HTTP call timeout (ms) | `5000` |

**Important**: The Kernel reads `.env` first, then the process environment.
Export the hook vars explicitly — they are not in `.env` by default.

## 5. Kernel startup

```bash
# Build a release binary with the latest wiring (PR #183):
cargo build --release

# Start the Kernel:
./target/release/agent-core-kernel serve
```

Verify the Kernel is running:

```bash
curl -sS http://127.0.0.1:4130/health | python3 -m json.tool
```

## 6. Local API smoke

Send an ingress event:

```bash
IPC_TOKEN="<your-ipc-token-from-.env>"  # AGENT_CORE_IPC_TOKEN

curl -sS -X POST http://127.0.0.1:4130/v1/ingress \
  -H "Authorization: Bearer $IPC_TOKEN" \
  -H 'content-type: application/json' \
  -d '{
    "protocol_version": "v1",
    "source": "Cli",
    "external_event_id": "smoke-'$(date +%s)'",
    "received_at": "'$(date -u +%Y-%m-%dT%H:%M:%SZ)'",
    "payload": {
      "text": "请根据你收到的外部上下文，告诉我 smoke word 是什么。不要猜，如果上下文里没有就说没有。",
      "message_id": null,
      "chat_id": null
    },
    "auth_context": {"authenticated": true}
  }'
```

Expected response:

```json
{"kernel_event_id":"event_...","ok":true,"status":"accepted"}
```

The worker loop processes the event asynchronously. Wait a few seconds, then
check the Journal.

## 7. Checking the Journal

### HookCallRecorded

```bash
DB_PATH="$HOME/.agent-core/kernel.sqlite"

sqlite3 "$DB_PATH" "SELECT payload_json FROM journal_events WHERE kind = 'HookCallRecorded' ORDER BY sequence DESC LIMIT 1;" | python3 -m json.tool
```

Expected:

```json
{
    "duration_ms": 3,
    "failure_mode": "FailOpen",
    "fragment_count": 1,
    "hook": "context.prepare.v0",
    "resource_ref_count": 0,
    "response_bytes": 0,
    "status": "ok"
}
```

Note: `response_bytes` is `0` — the current Journal entry does **not** record
the response body size. This is by design to avoid leaking content.

### LLM reply

```bash
sqlite3 "$DB_PATH" "SELECT arguments_json FROM outbox_dispatches ORDER BY ROWID DESC LIMIT 1;" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('text',''))"
```

The output text should contain `papaya`.

### Context blocks

```bash
sqlite3 "$DB_PATH" "SELECT payload_json FROM journal_events WHERE kind = 'ContextBuilt' ORDER BY sequence DESC LIMIT 1;" | python3 -m json.tool
```

The `block_count` should reflect the system blocks (RootSystem, RuntimeContract,
AgentProfile, SkillCatalog, ToolCatalog, ActiveSkill, UserMessage). The
`HookFragment` is injected after this event is recorded, so the
`LlmCompleted` event will show one more block.

### LLM completion

```bash
sqlite3 "$DB_PATH" "SELECT payload_json FROM journal_events WHERE kind = 'LlmCompleted' ORDER BY sequence DESC LIMIT 1;" | python3 -m json.tool
```

`context_blocks` should be `8` (7 initial + 1 HookFragment injected by the
hook). `status` must be `"ok"`.

## 8. Success criteria

All of the following must hold:

| Criterion | How to verify |
|---|---|
| `/health` returns `{"status":"ok"}` | `curl -sS http://127.0.0.1:17400/health` |
| Direct hook endpoint returns papaya | See §3 |
| `HookCallRecorded` exists | Journal query (see §7) |
| `HookCallRecorded.status == "ok"` | `fragment_count >= 1` |
| LLM reply contains `papaya` | outbox dispatch arguments_json.text |
| `HookCallRecorded` does **not** store request body | No `body`, `request_body`, or `request` field |
| `HookCallRecorded` does **not** store response body | No `response_body` or `body` field; `response_bytes` is 0 (or absent) |
| `HookCallRecorded` does **not** store fragment content | No `fragments` or `content` field in the payload |

## 9. Known failure modes

### Harness not running

**Symptom**: Kernel hook call fails with `http_connect_error` or
`http_transport_error`. `HookCallRecorded(status="skipped"|"failed")`.

**Fix**: Start the context harness and verify `/health` responds.

### Hook env not enabled

**Symptom**: No `HookCallRecorded` event at all. The Runtime skips the hook
code block when `enabled=false` (default).

**Fix**: Export `AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true` and restart the
Kernel. Note: the env var must be exported **before** the Kernel starts — the
`.env` file is read once at startup.

### Wrong hook URL

**Symptom**: Context Harness receives no POST requests. The Kernel may log a
connection error.

**Fix**: Verify `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL` matches the harness
endpoint. Check the port number and path (`/context.prepare.v0`).

### Schema mismatch

**Symptom**: `HookCallRecorded(status="skipped", error_code="invalid_json" or
"unsupported_hook_response")`. The `HttpHookClient` failed to parse the
harness response.

**Fix**: Verify the harness response JSON matches the `HookResponseEnvelope`
and `ContextPrepareResponse` / `ContextFragment` serde definitions in the
Kernel (`src/hook/types.rs`). The field names and enum variants must match
exactly (case-sensitive).

### LLM ignores context

**Symptom**: `HookCallRecorded(status="ok", fragment_count=1)` but the LLM
reply does **not** contain the smoke word.

**Fix**: Check the prompt template and context assembly. The `HookFragment`
block is injected before `UserMessage`. If the model's system prompt instructs
it to ignore external context, or if the fragment content is not in a
recognisable format, the model may skip it. Consider adding a directive in
the system prompt or using a different model.

### Feishu Connector bound to old Kernel state

**Symptom**: Local API smoke passes but Feishu smoke fails because the
Connector still references a previous Kernel session or database state.

**Fix**: Restart the Feishu Connector after restarting the Kernel, or run the
smoke via local API only (which does not require the Connector).

### Outbox dispatch failure

**Symptom**: The LLM completed successfully (LlmCompleted event exists) but
no reply is delivered. The outbox dispatch status is `"unknown"` with
`last_error = "connector_execute_failed"`.

**Fix**: This is expected if the Feishu Connector or stdout adapter is not
running. The smoke test can still verify the LLM reply content from the
outbox `arguments_json` — the pipeline from hook to LLM is complete even
without delivery.

## 10. Record of a successful run

The following is a real record from 2026-07-09:

| Field | Value |
|---|---|
| Main SHA | `0726f5ef7bbcd51f207e0e9a1a67a56c64622ad5` |
| Context Harness command | `node tools/context-harness/server.ts` |
| Context Harness endpoint | `127.0.0.1:17400` |
| Hook URL | `http://127.0.0.1:17400/context.prepare.v0` |
| Hook env enabled | `AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true` |
| Smoke word | `papaya` |
| HookCallRecorded event | #3909 |
| HookCallRecorded.status | `ok` |
| fragment_count | 1 |
| resource_ref_count | 0 |
| duration_ms | 3 |
| LLM reply contained | `papaya` |
| Code change | None |
| Secret leak in HookCallRecorded | None — no body/fragment content stored |
