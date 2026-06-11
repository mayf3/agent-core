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

Implementation status: in progress.

Done:

- health probe at `GET /health`;
- hash chain verify in health snapshot;
- unknown invocation scan from `DispatchStarted` without matching
  `ReceiptReceived`;
- `session.spawn` and `run.yield` return `not_enabled`.

Remaining:

- recent messages;
- context truncation;
- skill catalog from installed skill files;
- graceful shutdown;
- restart recovery.

## Phase 0 Non-Goals

Do not implement multi-agent, shell, memory, dynamic hooks, workflow graph,
approval wait states, scheduler, sandbox, or self-evolution.
