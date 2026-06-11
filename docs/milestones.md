# Agent Core Milestones

This file is the施工单. It deliberately excludes long-term protocol detail; see
[Architecture RFC](./architecture-rfc.md) for invariants and future contracts.

## Current Status

| Milestone | Status | Notes |
|---|---|---|
| Rust Phase 0 M0 | Done | Rust Kernel CLI vertical slice with SQLite Journal |
| Rust Phase 0 M1 | Done | TS Feishu long-connection connector + Rust Echo |
| Rust Phase 0 M2 | Done | Feishu text now uses Rust OpenAI-compatible LLM path |
| Rust Phase 0 M3a | Done | health probe, unknown scan, disabled spawn/yield ABI |
| Rust Phase 0 M3b | Done | file-backed context, skill catalog, recent messages, truncation |
| Rust Phase 0 M3c | Done | startup recovery marks unknown dispatches without mutating history |
| Rust Phase 0 M3d | Done | SIGINT/SIGTERM stops accepting new kernel connections gracefully |
| Cleanup | Done | legacy Node agent runtime packages removed |
| Rust Phase 0 M4a | Done | `/v1/ingress` returns accepted before background delivery finishes |

## Stage Plan

### Phase 0 M0: Rust CLI Vertical Slice

Goal:

```text
CLI text -> Gateway -> Runtime -> stdout Invocation -> Receipt -> Journal
```

Status: done.

### Phase 0 M1: Feishu Echo

Goal:

```text
Feishu long connection -> TS Connector -> Rust Kernel -> echo reply
```

Status: done. The connector is only an edge adapter. Rust owns Gateway,
Session, Run, Journal, and intent creation.

### Phase 0 M2: Feishu LLM Reply

Goal:

```text
Feishu text -> Context -> OpenAI-compatible provider -> reply original message
```

Status: done. The model chooses final text only; Runtime wraps it into a
current-session `feishu.send_message` intent and Gateway checks the target.

### Phase 0 M3: Reliability

Status: done.

- health probe;
- hash-chain verification;
- unknown invocation scan and startup recovery;
- file-backed context from `~/.agent-core`;
- skill catalog and active chat skill loading;
- recent-message context;
- graceful shutdown.

### Next: M4 Invocation Gateway Hardening

Before hardening broader invocation semantics, finish the remaining reliability
slice:

- durable worker queue for accepted events;
- durable outbox for connector dispatch;
- connector-local reaction outbox.

Scope:

- run principal
- per-channel execution profiles
- final system guard for approval resume
- clearer adapter timeout/error receipts

### Later: Plugin Registries

Scope:

- context contributor registry;
- trusted hook registry;
- external system manifests;
- out-of-process adapters.

### Later: Multi-Agent and Workflow

Scope:

- separate agent directories
- delegation packets
- external workflow source of truth
- command/query/event/receipt integration

### Later: Bounded Self-Evolution

Scope:

- git worktree candidate
- selected historical run replay
- evaluator script producing `score.json` and `report.md`
- promote through PR merge
- rollback to last-known-good tag

## Near-Term Rule

Do not add general hook runtime, skill runtime, external system registry, or heavy
sandbox before M4 and M5 prove the repeated shapes.

## Rust Kernel Reset

The implementation direction is now frozen as TypeScript Feishu Connector plus
Rust Kernel. The legacy Node runtime packages have been removed. New Runtime,
Gateway, Journal, Session, Run, Context, and Invocation approval work goes into
the Rust Kernel. TypeScript remains only for the Feishu edge connector.

See [Phase 0 Construction Plan](./phase0-plan.md).
