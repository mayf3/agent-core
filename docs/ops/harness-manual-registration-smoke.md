# Harness Manual Registration and Smoke

> **Audience**: Agent Core operators and developers
> **Prerequisites**:
> - Kernel is built and running (`cargo run`)
> - Coding Harness is configured with `harness-dev` workspace
> - External Harness workspace exists at `~/.agent-core/harnesses/`

## Overview

This runbook describes the **manual registration and smoke verification**
process for connecting an external Harness to the Kernel via the
`context.prepare.v0` hook.

In v0, registration is entirely manual:
1. The Harness is developed in `~/.agent-core/harnesses/<id>/`
2. The operator sets environment variables and restarts the Kernel
3. No automatic discovery, no manifest parser, no Dashboard

## Workflow

### 1. Create the Harness workspace

```bash
mkdir -p ~/.agent-core/harnesses/context-markdown-harness
```

### 2. Write the manifest

Create `~/.agent-core/harnesses/context-markdown-harness/harness.manifest.json`.

See the [example manifest](../../tools/coding-harness/examples/harness.manifest.example.json)
and the [Harness Manifest v0 spec](../architecture/harness-manifest-v0.md).

### 3. Write the Harness server code

```bash
cd ~/.agent-core/harnesses/context-markdown-harness
npm init -y
npm install express
cat > server.ts <<'EOF'
import express from 'express';
const app = express();
app.use(express.json());

app.get('/health', (_req, res) => { res.json({ status: 'ok' }); });

app.post('/context.prepare.v0', (req, res) => {
  const { request_id } = req.body;
  res.json({
    protocol_version: 'hook-abi-v0',
    hook: 'context.prepare.v0',
    request_id,
    success: true,
    fragments: [{
      id: 'markdown-context',
      hook_id: 'context-markdown-harness',
      kind: 'fact',
      placement: 'user_context',
      priority: 100,
      content: 'Smoke word: papaya. User context: SOUL.md + USER.md + project_facts.md',
      source: 'context-markdown-harness',
      ttl_secs: 300,
      estimated_tokens: 50,
      sensitivity: 'public',
    }],
  });
});

app.listen(17400, '127.0.0.1', () => console.log('ready on 17400'));
EOF
```

### 4. Run Harness tests

```bash
cd ~/.agent-core/harnesses/context-markdown-harness
npm test
```

If the manifest includes `smoke.command`, run that:

```bash
npm test
echo "Exit code: $?"   # Must be 0
```

### 5. Start the Harness server

```bash
cd ~/.agent-core/harnesses/context-markdown-harness
node server.ts &
```

### 6. Health check

```bash
curl -s http://127.0.0.1:17400/health | jq .
```

Expected:

```json
{
  "status": "ok"
}
```

If the health endpoint is unreachable or returns a non-200 status, fix the
Harness before proceeding.

### 7. Hook endpoint smoke

Send a test request to the hook endpoint:

```bash
curl -s -X POST http://127.0.0.1:17400/context.prepare.v0 \
  -H 'Content-Type: application/json' \
  -d '{
    "protocol_version": "hook-abi-v0",
    "hook": "context.prepare.v0",
    "request_id": "smoke-001",
    "payload": {}
  }' | jq .
```

Expected:

```json
{
  "protocol_version": "hook-abi-v0",
  "hook": "context.prepare.v0",
  "request_id": "smoke-001",
  "success": true,
  "fragments": [...]
}
```

### 8. Configure Kernel hook environment

Set the following environment variables and restart the Kernel:

```bash
export AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true
export AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0
export AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open
export AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS=5000
```

### 9. Restart the Kernel

```bash
# Stop the running Kernel process (Ctrl+C or kill)
# Then restart with the new env:
AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true \
AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0 \
AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open \
AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS=5000 \
cargo run
```

On startup, verify the log includes:

```
[runtime] context.prepare.v0 hook: enabled → http://127.0.0.1:17400/context.prepare.v0
```

### 10. Kernel smoke run

Send a message to the Kernel that should trigger the hook:

```bash
# Using the Kernel API (adjust URL and payload as needed)
curl -s -X POST http://127.0.0.1:4130/... \
  -H 'Content-Type: application/json' \
  -d '{
    "channel": { "kind": "cli" },
    "principal": { "kind": "user", "id": "smoke-tester" },
    "message": { "text": "请根据外部上下文回答 smoke word。" }
  }'
```

### 11. Verify LLM response

Check that the LLM response reflects the injected context. For the example
above, the response should mention "papaya" or reference the markdown context.

### 12. Verify Journal

Check the Journal for `HookCallRecorded` events:

```bash
# Query the Kernel journal (adjust path as needed)
sqlite3 ~/.agent-core/kernel/journal/journal.db \
  "SELECT COUNT(*) FROM events WHERE kind = 'HookCallRecorded' AND status = 'success';"
```

Expected: `1` (or more, depending on how many runs were made).

If the count is 0 or the status is `failed`, investigate the hook.

## Smoke verification checklist

| Step | Command | Expected | Pass/Fail |
|------|---------|----------|-----------|
| 1. Tests pass | `npm test` | Exit code 0 | |
| 2. Server running | `ps aux \| grep server.ts` | Process exists | |
| 3. Health check | `curl /health` | HTTP 200, `{"status":"ok"}` | |
| 4. Hook endpoint | `curl /context.prepare.v0` | HTTP 200, `success: true` | |
| 5. Kernel log | Check startup log | Hook enabled line | |
| 6. LLM response | Send smoke prompt | Response contains expected word | |
| 7. Journal | Query `HookCallRecorded` | At least 1 with `success` | |

## Rollback

### Health check failure at registration

If `curl /health` fails:

1. Fix the Harness server code
2. Restart the Harness server
3. Re-run health check
4. Do **not** set Kernel hook env until health passes

### Hook endpoint returns errors

If the hook endpoint returns non-200 or `success: false`:

1. Fix the Harness server code
2. Restart the Harness server
3. Re-run hook endpoint check
4. Do **not** set Kernel hook env until endpoint responds correctly

### Kernel RunFailed after hook enablement

If the Kernel starts crashing or runs fail after enabling the hook:

1. Restore the previous hook URL:

```bash
# If you saved the previous URL:
export AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0
# Or disable the hook:
export AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=false
```

2. Restart the Kernel
3. Verify runs succeed again
4. Diagnose the Harness issue

### HookCallRecorded failures increase

If monitoring shows increasing `HookCallRecorded` with status `failed`:

1. Disable the hook:

```bash
export AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=false
```

2. Restart the Kernel
3. Check the Harness logs for errors
4. Fix and re-test before re-enabling

### LLM does not consume expected context

If the LLM response does not reflect the injected context:

1. This is **not** an automatic rollback trigger
2. Check the Journal for `HookCallRecorded` to confirm the hook was called
3. Check the context fragments in the Journal to confirm they were injected
4. If fragments were injected but the LLM ignored them, adjust the fragment
   content, priority, or placement
5. If fragments were not injected, fix the hook server

### Core principle

```
Rollback should prioritize disabling the hook,
not blindly switching to an old endpoint.
```

A disabled hook can be re-enabled after root cause analysis. An old endpoint
may itself be faulty or incompatible with the current Kernel version.

## Troubleshooting

### Harness server won't start

- Check port availability: `lsof -i :17400`
- Check Node.js version: `node --version`
- Check dependencies: `npm install`

### Kernel does not call the hook

- Verify the env var is set: `echo $AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED`
- Verify the Kernel was restarted after setting the env var
- Check the Kernel startup log for hook configuration

### HookCallRecorded not in Journal

- The hook may not have been triggered (no Run consumed the hook)
- Send a message that triggers a Run
- Check that the hook is enabled in the Kernel config

### Hook returns timeout

- Check that the Harness server is running and reachable
- Check for network issues (firewall, loopback interface)
- Increase `AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS` if the hook needs more time

---

*End of document.*
