# Decision: Tool-Call Execution Loop — smallest safe design

## Status

Design document. No `src/` change. **Requires maintainer sign-off before
implementation begins** — see §4.6.

## 1. Problem

PR #99 (`catalog_for_context()` + `ContextBlockKind::ToolCatalog`) made the
operation catalog visible to the model as a context block. However, there is
**no mechanism for the model to emit a tool call that becomes an
`InvocationIntent`**. The model still produces free-text replies; the Kernel
cannot parse structured tool calls from model output.

The gap: the catalog is *visible* but not *actionable*.

## 2. Scope

This design covers the **smallest safe increment**: making exactly one
read-only operation (`time.now`) executable through a model-emitted tool call.
It proves the pipeline and rejects everything else.

**In scope (MVP):**

- Parse a model-emitted tool-call from the LLM response.
- Convert it into an `InvocationIntent`.
- Route it through the existing `intent → policy → adapter → receipt` chain.
- Return the receipt output to the model/user in the next turn.
- Reject unknown/generated operations before adapter execution.
- Hard error on `Write` operations — only `ReadOnly` (`time.now`) may execute
  via this path in MVP.

**Out of scope (MVP):**

- Shell, browser, workflow, memory, deployment, arbitrary HTTP, or
  unregistered tools.
- Multi-turn tool orchestration (loops, retries, conditionals).
- Approval UI for Write operations via model-initiated calls.
- Streaming tool-call responses.
- Parallel tool calls.
- `feishu.send_message` / `stdout.send_text` via tool call (they remain
  Write; a future phase may add Write tool-call execution with approval).

## 3. Current State (main)

### 3.1 What exists

- **Operation catalog** (`src/domain/operation.rs`): `CATALOG` array with
  three operations: `stdout.send_text` (Write), `feishu.send_message` (Write),
  `time.now` (ReadOnly). `lookup()` returns `None` for unknown operations.
  `catalog_for_context()` renders the catalog as text for the LLM context.
- **ToolCatalog context block** (`src/context.rs`): `tool_catalog_block()`
  wraps `catalog_for_context()` in a `ContextBlockKind::ToolCatalog` block.
  The block tells the model "Available operations (propose via the kernel;
  read-only ones run inline, write ones need approval when enabled)."
- **Context assembly** (`src/context.rs`): `build_context()` assembles all
  blocks (RootSystem, RuntimeContract, AgentProfile, SkillCatalog,
  ToolCatalog, ActiveSkill, RecentMessages, UserMessage).
- **LLM client** (`src/llm/mod.rs`): Returns `LlmOutput { content: String }`.
  The content is the model's free-text reply. No tool-call parsing exists.
- **LLM serialization** (`src/llm/mod.rs`): `serialize_system_context()`
  renders context blocks as `## ToolCatalog\n...` sections.
- **Runtime** (`src/runtime/mod.rs`): `Runtime::deliver()` is the main entry
  point that takes a `ValidatedEvent`, builds context, calls the LLM,
  and produces outbox dispatches. It does not inspect model output for
  structured tool calls.

### 3.2 What is missing

1. No way for the model to signal "I want to execute operation X with args Y."
2. No way to parse such a signal from model output.
3. No way to attach the resulting receipt back to the conversation.
4. No tests covering the tool-call → intent → execution → receipt round trip.

### 3.3 Key constraint

The model provider API (OpenAI-compatible) already supports
`function_call`/`tool_calls` in the response. However, the Kernel currently
treats `content` as a plain string. Making the model emit an OpenAI
`function_call` is the natural path — but it requires the Kernel to:

- Include `functions` or `tools` in the request payload.
- Parse `function_call` / `tool_calls` from the response.
- Map the parsed call back to catalog operations.

## 4. Proposed Design

### 4.1 Overview

```text
LlmOutput.content (text reply)
  or LlmOutput.tool_call (structured)   ← NEW
       ↓
parse into InvocationIntent             ← NEW
       ↓
lookup(operation) → is_allowed?
       ↓
is ReadOnly?                            ← MVP gate: only ReadOnly passes
       ↓ (yes)
validate_tool_call → InvocationProposed → InvocationApproved (sync)
       ↓
TimeAdapter::execute() inline           ← NOT queued into outbox
       ↓
ReceiptReceived journaled              ← OutboxQueued/DispatchStarted skipped
       ↓
Append receipt output as ToolResult context block
```

### 4.2 Data flow

**Step 1 — LLM request modification**

The Kernel includes `functions` or `tools` in the chat completion request.
Only catalog operations with `Risk::ReadOnly` are surfaced as tool definitions
in the MVP (i.e., just `time.now`). Write operations are excluded from the
request payload — they cannot be called because the model never receives their
schemas.

Implementation sketch (not actual Rust):

```
fn tools_for_request() -> Vec<ToolDefinition> {
    CATALOG
        .iter()
        .filter(|spec| spec.risk == Risk::ReadOnly)
        .map(|spec| ToolDefinition {
            name: spec.name.to_string(),
            description: match spec.name {
                TIME_NOW => "Read the current kernel wall-clock time. No arguments, no side effects.",
                _ => "",
            },
            parameters: match spec.name {
                TIME_NOW => serde_json::json!({"type": "object", "properties": {}}),
                _ => serde_json::json!({"type": "object", "properties": {}}),
            },
        })
        .collect()
}
```

**Step 2 — LLM response parsing**

`LlmOutput` gains an optional `tool_call` field:

```rust
pub struct LlmOutput {
    pub provider: String,
    pub model: String,
    pub content: String,         // unchanged
    pub tool_call: Option<ToolCall>,  // NEW
    pub journal_payload: serde_json::Value,
}

pub struct ToolCall {
    pub id: String,
    pub operation: String,       // must match CATALOG
    pub arguments: serde_json::Value,
}
```

The LLM client parses `function_call` / `tool_calls` from the API response.
If absent, `tool_call` is `None` (fallback to text-only flow).

**Step 3 — Tool-call validation (new `src/gateway/tool_call.rs`)**

```
fn validate_tool_call(tool_call: ToolCall) -> Result<InvocationIntent> {
    let spec = operation::lookup(&tool_call.operation)
        .ok_or(ToolCallError::UnknownOperation)?;
    if spec.risk != Risk::ReadOnly {
        return Err(ToolCallError::WriteOperationNotAllowed)?;
    }
    // Build an InvocationIntent from the tool call
    Ok(InvocationIntent {
        invocation_id: InvocationId::new(),
        run_id: ???,            // see §4.4
        operation: tool_call.operation,
        arguments: tool_call.arguments,
        idempotency_key: Some(format!("tool:{}", tool_call.id)),
    })
}
```

**Step 4 — Inline execution (MVP path)**

For `time.now` (a local `TimeAdapter`), the validated `InvocationIntent` is
**not queued into `outbox_dispatches`**. The outbox dispatcher is wired to a
single `HttpConnectorAdapter` (`src/server/delivery.rs:164`), which sends
invocations over HTTP to the connector. Queuing a local adapter's intent there
would route it through the wrong adapter.

Instead, the MVP executes `TimeAdapter` **inline** in the Runtime's delivery
path after intent+policy approval:

1. `validate_tool_call()` returns an `InvocationIntent`.
2. The Runtime creates the `InvocationProposed` and `InvocationApproved`
   Journal events (mirroring the existing approval path synchronously for
   `ReadOnly`).
3. `TimeAdapter::execute()` runs inline — it reads `Utc::now()` and returns a
   `Receipt`.
4. A `ReceiptReceived` Journal event is written.
5. **`OutboxQueued` and `DispatchStarted` are intentionally skipped** — they
   are outbox-dispatch lifecycle events that do not apply to inline local
   adapters.
6. The receipt output is attached to the context as a `ToolResult` block.

An alternative (post-MVP) is to add an explicit **adapter router** that maps
operation names to adapter implementations (local vs HTTP). The router would
live in `src/runtime/` and allow both `TimeAdapter` (inline) and
`HttpConnectorAdapter` (outbox) to coexist. The MVP chooses the simpler inline
path and defers the router to a follow-up increment.

**Step 5 — Context feedback**

The receipt output is appended to the next context as a new context block
(e.g. `ContextBlockKind::ToolResult`). On the next model call, the model
sees the result of its tool call and can respond verbally.

### 4.3 Run identity for tool-call intents

A tool call does not have its own `Run` — it is an action *within* the
current run. Options:

| Option | Pros | Cons |
|---|---|---|
| A: Create a sub-run | Clean journal fact isolation | Copies run lifecycle complexity; many sub-runs for simple reads |
| B: Attach to current run as another intent | Simple; reuses existing intent/adapter/receipt | Intent lifecycle tied to parent run's status |
| C: Inline execution in the model loop | Simplest; no run creation | Breaks the intent → receipt journal invariant |

**Recommendation: Option B (attach to current run).**

The tool call creates an `InvocationIntent` associated with the current run
ID. The receipt is journaled as a fact of that run. This matches the existing
pattern where a single run may produce multiple outbox dispatches (e.g., a
reply message). It is the smallest delta from the current code.

For the MVP, `time.now` is ReadOnly and produces no durable side effect
outside the journal, so Option B is safe. If future Write tool calls need
approval, the existing `AwaitingApproval` pause mechanism in the run covers
that (the run pauses, approval resolves, dispatches continue).

### 4.4 Rejection paths

| Condition | Behaviour |
|---|---|
| `tool_call.operation` not in `CATALOG` | Return error to context: "Operation X is not in the catalog." Do not execute. Log as `ToolCallRejected` fact. |
| `tool_call.operation` is `Write` (e.g. `feishu.send_message`) | Return error: "Write operations cannot be initiated via tool call in this phase." Do not execute. |
| `tool_call.arguments` fails schema validation | Return error: "Invalid arguments for operation X." Do not execute. |
| Model returns both `content` and `tool_call` | Execute the tool call; append result to context alongside content. If both present, content is treated as the model's verbal response, tool_call as the action. |
| Model returns multiple tool calls | MVP: execute only the first. Log a warning that parallel calls are not yet supported. |

### 4.5 Audit facts for inline tool-call execution

To preserve the `intent → decision → receipt` audit trail, the following
Journal events are written for a successful `time.now` tool call:

| Journal event kind | Purpose |
|---|---|
| `InvocationProposed` | Records that the model emitted a tool call for a catalog operation. Payload includes the operation name, arguments, and the originating run/session. |
| `InvocationApproved` | Records that the intent passed policy (always approved synchronously for `ReadOnly` in MVP). |
| `ReceiptReceived` | Records the `TimeAdapter` output (`iso`, `epoch_ms`) and `ReceiptStatus::Succeeded`. |

**Explicitly skipped** for inline local adapters:

| Journal event kind | Reason |
|---|---|
| `OutboxQueued` | Not queued — the inline path bypasses `outbox_dispatches`. |
| `DispatchStarted` | No outbox dispatch lifecycle exists for inline execution. |

If the tool call fails (e.g. adapter error), a `ReceiptReceived` with
`ReceiptStatus::Failed` is written instead, and the error is surfaced in the
`ToolResult` context block.

### 4.6 Implementation sign-off gate

This design modifies `src/` files (the Rust Kernel). **No implementation must
start until this design document is merged and the maintainer explicitly signs
off on implementation.** See `docs/agent-dispatch.md` §Roles: the maintainer
decides when a phase or increment is allowed to start.

## 5. Files to change (implementation)

| File | Change |
|---|---|
| `src/llm/mod.rs` | Add `ToolCall` struct and `tool_call: Option<ToolCall>` to `LlmOutput`. Parse `function_call`/`tool_calls` from OpenAI response. Add `tools` to request payload (only ReadOnly operations in MVP). The OpenAI request/response logic lives here (there is no separate `src/llm/openai.rs` in this repo). |
| `src/gateway/mod.rs` or new `src/gateway/tool_call.rs` | Add `validate_tool_call()` — checks operation exists in catalog, risk is ReadOnly, arguments are valid. Returns `InvocationIntent`. |
| `src/runtime/mod.rs` | After `llm_client.complete()`, check `LlmOutput.tool_call`. If present, validate, execute inline via `TimeAdapter` (not outbox), write `InvocationProposed` / `InvocationApproved` / `ReceiptReceived` Journal events, collect receipt. |
| `src/context.rs` | Add `ContextBlockKind::ToolResult` variant. After tool execution, append receipt output as a `ToolResult` block. |
| `src/domain/mod.rs` | Add `ContextBlockKind::ToolResult` to the enum. |

## 6. Tests needed

| Test | Description |
|---|---|
| `tool_call_valid_operation` | `validate_tool_call` accepts a valid `time.now` call. |
| `tool_call_unknown_operation` | `validate_tool_call` rejects `shell.exec`. |
| `tool_call_write_operation_rejected` | `validate_tool_call` rejects `feishu.send_message` (Write). |
| `tool_call_invalid_arguments` | `validate_tool_call` rejects `time.now` with unexpected arguments (e.g. `{"zone": "UTC"}`). |
| `tool_call_round_trip` | End-to-end: model emits `time.now` → intent → adapter → receipt → context result. |
| `catalog_read_only_operations_exposed_as_tools` | `tools_for_request()` includes `time.now` but not `feishu.send_message`. |
| `model_text_fallback` | If model returns no tool_call, the text-only path works unchanged. |
| `model_tool_call_parallel` | If model returns multiple tool_calls, only the first executes and a warning is logged. |

## 7. Not breaking

- The text-only path (current flow) is untouched. If the model does not emit
  a tool call, behavior is identical to today.
- The ToolCatalog context block text remains unchanged — it tells the model
  about operations. The tool mechanism is the structured counterpart.
- No schema migration, no new SQLite tables, no new DB state.
- No change to the Gateway approval flow for model-initiated tool calls
  (they are synchronous for ReadOnly; Write tool calls are rejected before
  reaching the adapter).
- The outbox dispatcher (`src/server/delivery.rs`) is untouched — inline
  tool-call execution bypasses `outbox_dispatches` entirely.

## 8. Security & Boundary

- Only `ReadOnly` operations are exposed as tool definitions. The model cannot
  call `Write` operations through this path because their schemas are never
  sent in the request — and even if a model hallucinates a tool name matching
  a Write operation, `validate_tool_call` rejects it.
- Unknown operation names are rejected before any adapter is looked up.
- Arguments are JSON, parsed by `serde_json`. The `TimeAdapter` ignores them
  in the MVP, but future adapters should validate against their schema.
- No shell escape, no file system access, no network calls are added to the
  Kernel by this change. `TimeAdapter` already exists and is safe.

## 9. Future increments (after MVP)

1. **Write tool-call execution with approval**: expose Write operations as
   tool definitions, but route them through the existing approval pause
   mechanism. The run pauses, the operator approves or denies, and the tool
   executes (or is rejected).
2. **Result streaming**: stream tool execution progress back to the
   conversation.
3. **Parallel tool calls**: execute multiple tool calls in a single turn.
4. **Tool-call retry**: if a tool call fails with a transient error, the
   runtime may retry.
5. **Argument schema validation**: enforce JSON Schema for each tool's
   arguments before execution.
6. **Tool-call history in journal**: persist tool calls as first-class
   journal facts for auditability.

## 10. Implementation order

1. Add `ToolCall` struct and `tool_call: Option<ToolCall>` to `LlmOutput`.
2. Modify the OpenAI request builder to inject ReadOnly operations as
   `functions`/`tools`.
3. Parse `function_call`/`tool_calls` from the response.
4. Add `validate_tool_call()` in a new gateway module.
5. Add `ToolResult` context block kind.
6. Wire the tool-call flow into `Runtime::deliver()` (or a new method).
7. Write tests from §6.
8. Update docs.
