# External Context Harness v0

A minimal local HTTP server that implements the `context.prepare.v0` hook
endpoint for the Agent Core Kernel.

## What this is

This is a **development/verification tool** that the Kernel's Runtime calls
(via `HttpHookClient`) to inject an external context fragment before each
LLM completion. In v0 it returns a **fixed** context fragment:

    EXTERNAL_CONTEXT_SMOKE_WORD: papaya

This lets you verify the full end-to-end pipeline:

    Kernel → context.prepare.v0 HTTP hook → Context Harness → ContextFragment → LLM prompt

## What this is NOT

This is **not** a Memory, Skill, Task, Dream, or Workspace system. It does
not persist data, does not authenticate callers, and does not implement any
product-layer logic. It is a smoke-test tool only.

## How to start

```bash
# From the repo root:
node tools/context-harness/server.ts

# Or with a custom port:
PORT=17400 node tools/context-harness/server.ts

# Or via npm:
npm run context-harness
```

The server listens on `127.0.0.1:17400` by default.

## Kernel environment configuration

Add these to your `.env` or export them before starting the Kernel:

```bash
export AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true
export AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0
export AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open
export AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS=3000
```

## Endpoints

### `GET /health`

Returns `{"status":"ok"}`. Use for readiness checks.

### `POST /context.prepare.v0`

Accepts a `HookRequestEnvelope` JSON body. Returns a `HookResponseEnvelope`
with a single `ContextFragment` containing `EXTERNAL_CONTEXT_SMOKE_WORD: papaya`.

Request format (see `HookRequestEnvelope` in the Kernel):

```json
{
  "hook": "context.prepare.v0",
  "request_id": "...",
  "timestamp": "...",
  "payload": { "...": "ContextPrepareRequest fields" }
}
```

Response format (see `HookResponseEnvelope` in the Kernel):

```json
{
  "request_id": "<same request_id>",
  "hook": "context.prepare.v0",
  "timestamp": "<rfc3339>",
  "payload": {
    "fragments": [
      {
        "id": "frag_...",
        "hook_id": "context.prepare.v0",
        "kind": "fact",
        "placement": "user_context",
        "priority": 1,
        "content": "EXTERNAL_CONTEXT_SMOKE_WORD: papaya",
        "source": "context-harness:v0",
        "ttl_secs": null,
        "estimated_tokens": 10,
        "sensitivity": "internal"
      }
    ],
    "resource_refs": []
  }
}
```

## How to smoke-test

1. Start the context harness:
   ```bash
   node tools/context-harness/server.ts
   ```

2. In another terminal, start the Kernel with the env vars above.

3. Send a message to the agent, e.g.:
   ```
   "请根据你收到的外部上下文，告诉我 smoke word 是什么"
   ```

4. The LLM reply should include `papaya` (or mention the external context).
   The Journal will contain `HookCallRecorded(status=ok)`.

## Security

- **Local development only.** Bind to `127.0.0.1` by default.
- No authentication or tokens in v0.
- Does not log request bodies or full headers.
- Does not persist any user content.
