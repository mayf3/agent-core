# Phase 0 Construction Plan

This document records the frozen Phase 0 implementation decisions. It is a
施工单, not a new architecture proposal.

## Goal

Phase 0 validates one minimal, non-bypassable loop:

```text
IngressEnvelope
-> Gateway
-> ValidatedEvent
-> Runtime.event.deliver
-> Session
-> Run
-> Context
-> LLM
-> InvocationIntent
-> Gateway
-> ApprovedInvocation
-> Adapter
-> Receipt
-> Journal
```

The first user-visible channel is Feishu, but M0 starts with a Rust CLI vertical
slice so the kernel boundary is proven before the connector is added.

## Technology ADR

Decision:

```text
TypeScript Feishu Connector + Rust Kernel
```

Rust Kernel is the only owner of Runtime, Gateway, Journal, Session, Run,
Context, Policy, Invocation approval, and durable state.

TypeScript Connector is only the Feishu edge adapter. It may handle Feishu long
connection, auth, event parsing, and sending, but it must not own Session, LLM,
Context, Policy, Agent Loop, or Journal.

Phase 0 uses local HTTP JSON IPC:

- `POST http://127.0.0.1:{kernel_port}/v1/ingress`
- `POST http://127.0.0.1:{connector_port}/v1/execute`

IPC requirements:

- bind only to `127.0.0.1`;
- shared Bearer token;
- required `protocol_version`;
- request/invocation IDs on every request;
- strict schema and operation allowlist;
- no token, app secret, full Authorization header, or raw secret payload in logs;
- explicit timeouts;
- retries reuse the same idempotency key.

Phase 0 fixes Feishu long connection as the only Feishu ingress mode. Webhook is
out of scope.

## Storage

M0 uses SQLite from the first CLI message. SQLite is not a later reliability
feature; it is part of the kernel.

Runtime data defaults to `~/.agent-core`, not the source checkout. The kernel
creates missing default Agent documents there on startup and never overwrites
user edits. The source repository owns code and bootstrap defaults only.

Tables:

- `sessions`
- `runs`
- `journal_events`
- `ingress_dedup`

`journal_events` includes `correlation_id` so invocation status can be derived
without parsing every payload.

Journal append is serialized with a single writer and `BEGIN IMMEDIATE`:

```text
BEGIN IMMEDIATE
-> read latest sequence/hash
-> canonical payload
-> compute hash
-> insert event
-> COMMIT
```

## Core Types

Phase 0 core types:

- `Agent`
- `Session`
- `RunPrincipal`
- `Run`
- `IngressEnvelope`
- `ValidatedEvent`
- `ContextBlock`
- `InvocationIntent`
- `ApprovedInvocation`
- `Receipt`
- `JournalEvent`

All IDs use Rust newtypes to avoid mixing AgentId, SessionId, RunId, EventId,
InvocationId, and PrincipalId.

## Feishu Identity

Phase 0 does not guess ID types.

Config uses explicit allowlist entries:

```yaml
allowlist:
  users:
    - type: open_id
      value: ou_xxx
  chats:
    - type: chat_id
      value: oc_xxx
```

User principal:

```text
feishu:open_id:{open_id}
```

Chat principal:

```text
feishu:chat_id:{chat_id}
```

`union_id` and `user_id` are not primary identifiers in Phase 0.

## LLM Reply

Phase 0 does not require LLM tool calling for normal replies.

```text
LLM final text
-> Runtime wraps feishu.send_message or stdout.send_text InvocationIntent
-> Gateway fills and checks current session target
-> Adapter executes ApprovedInvocation
```

The model chooses content only. It cannot choose another recipient.

## Skill Loading

Phase 0 does not implement skill routing.

- Agent manifest names the default skill.
- Main agent loads `chat/SKILL.md`.
- Context includes skill catalog names and descriptions.
- Active skill loads full `SKILL.md`.
- Skill scripts are not executable and are not registered as tools.

## Secret Journal Rules

Use field allowlists, not write-then-redact.

Never record:

- app secret;
- tenant access token;
- Authorization header;
- cookie;
- IPC bearer token;
- environment variables;
- complete HTTP headers;
- complete SDK error objects.

Ingress journal may record normalized source, external event ID, sender open ID,
chat ID, message ID, message type, user text, time, and payload hash.

Adapter errors may record HTTP status, Feishu error code, request ID, and short
sanitized messages.

LLM errors may record provider, model, status, request ID, and error category.

Provider-specific model aliases may be normalized before sending the request.
For the Z.AI endpoint, `zai/glm-5.1` and `z.ai/glm-5.1` are normalized to
`glm-5.1`; generic OpenAI-compatible endpoints keep slash-prefixed model names
unchanged because providers such as aggregators may require them.

Phase 0 supports one optional OpenAI-compatible fallback endpoint through
`AGENT_CORE_FALLBACK_OPENAI_BASE_URL`, `AGENT_CORE_FALLBACK_OPENAI_API_KEY`, and
`AGENT_CORE_FALLBACK_MODEL`. Fallback is attempted once when the primary model
request fails or is not configured. This is not a provider registry or model
router.

## Milestones

### M0: Rust CLI Vertical Slice

```text
CLI
-> IngressEnvelope
-> Gateway
-> ValidatedEvent
-> event.deliver
-> Session
-> Run
-> Context
-> LLM
-> stdout InvocationIntent
-> Gateway
-> stdout Adapter
-> Receipt
-> Journal
```

### M1: TypeScript Feishu Connector + Echo

```text
Feishu long connection
-> TS Connector
-> Rust /v1/ingress
-> event.deliver
-> fixed reply intent
-> Rust Gateway
-> TS /v1/execute
-> Feishu
```

Implementation status: done. M1 keeps the Connector as an edge adapter:

- Feishu long connection uses `@larksuiteoapi/node-sdk` `WSClient`;
- Connector normalizes `im.message.receive_v1` into `/v1/ingress`;
- Connector returns quickly from the Feishu event callback;
- Rust Kernel owns Gateway, Session, Run, Journal, and echo intent creation;
- Connector `/v1/execute` only accepts `feishu.send_message`.
- Connector may add one best-effort processing reaction on the source message
  and remove it after `feishu.send_message` succeeds. This is connector-local
  UX state, tracked in memory by `message_id -> reaction_id`. It is not a model
  tool, workflow state, or Core Journal fact.
- If an accepted run or reply dispatch fails, Connector may replace the
  processing reaction with a configured failed reaction.
- Processing reactions must never run as a keepalive loop. Feishu reactions are
  persistent, so one add and one delete per handled message is the intended
  upper bound.

### M2: Feishu LLM Reply

Replace fixed reply with real Context + LLM. Reply still uses
`InvocationIntent -> ApprovedInvocation -> Adapter`.

Implementation status: done. M2 keeps the small-kernel boundary:

- Rust Kernel owns the OpenAI-compatible model call;
- Context uses the fixed Phase 0 blocks: root, runtime contract, main agent,
  skill catalog, active chat skill, and current user message;
- Model output is final reply text only;
- Runtime wraps that text into a current-session `feishu.send_message` intent;
- Gateway still verifies capability and target session before dispatch;
- LLM journal payload records provider, model, status, usage, and error
  category only, never prompts, API keys, or HTTP headers.

### M3: Reliability

Implementation status: done.

Done:

- health probe at `GET /health`;
- hash chain verify in health snapshot;
- unknown invocation scan from `DispatchStarted` without matching
  `ReceiptReceived`;
- `session.spawn` and `run.yield` return `not_enabled`.
- context loads `system/root.md`, `system/runtime.md`, `agents/main/AGENT.md`,
  and active `skills/chat/SKILL.md` from the runtime data dir;
- skill catalog is derived from installed `skills/*/SKILL.md` files;
- recent user messages are reconstructed from Journal for the current Session;
- truncation applies to compressible ContextBlocks.
- restart recovery marks old dispatched invocations without receipts as
  `ReceiptReceived` with `Unknown` status and fails the run.
- graceful shutdown stops accepting new connections on SIGINT/SIGTERM.

### M4: Async Ingress

Implementation status: minimal in-process slice.

Done:

- `/v1/ingress` validates, deduplicates, records `IngressAccepted`, and returns
  `accepted` with `kernel_event_id` before model execution finishes;
- actual `Runtime.event.deliver` runs on a background thread;
- startup scans `IngressAccepted` events that have no matching
  `SessionReady`/`RunStarted`/`RunCompleted` correlation and requeues the ones
  that can be reconstructed from whitelisted Journal fields;
- Feishu reaction cleanup remains bound to `/v1/execute` success, so it still
  works when ingress returns early.

Not done:

- separate worker queue table;
- graceful shutdown draining for background delivery workers;
- durable connector UX outbox for reaction retry.

## Phase 0 Non-Goals

Do not implement multi-agent, shell, memory, dynamic hooks, workflow graph,
approval wait states, scheduler, sandbox, or self-evolution.
