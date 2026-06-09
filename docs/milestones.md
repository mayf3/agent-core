# Agent Core Milestones

This file is the施工单. It deliberately excludes long-term protocol detail; see
[Architecture RFC](./architecture-rfc.md) for invariants and future contracts.

## Current Status

| Milestone | Status | Notes |
|---|---|---|
| M0 Bootstrap | Done | repo, checks, docs, PR-like merge history |
| M1 Kernel Records | Done | runs/events/state/envelope/doctor |
| M2 Tools + Approval | Done | built-in tools, policy, approval pause/resume |
| M3 Model + Agent Loop | Done | OpenAI-compatible provider, single agent loop |
| M4a Feishu Echo | Done | receive/normalize/filter/dedupe/reply echo core |
| M4b Feishu LLM Reply | Next | chat-only Feishu entry into agent loop |

## Stage Plan

### M4a: Feishu Echo

Goal:

```text
Feishu message -> normalize -> policy -> dedupe -> echo reply
```

Scope:

- Feishu config loader
- normalized inbound message type
- allowlist policy
- group mention policy
- bot self-message filter
- message id dedupe
- echo reply runtime
- tests with fake Feishu client

Out of scope:

- real LLM reply
- tool exposure from Feishu
- SQLite recovery
- cards, files, images, voice
- dynamic hook registry

Status: implemented as a testable transport core with fake client. Real Feishu
SDK connection is intentionally separate.

### M4b: Feishu LLM Reply

Goal:

```text
Feishu text -> main agent -> model provider -> reply original message
```

Scope:

- chat-only execution profile
- no filesystem or shell tools visible by default
- friendly model errors
- response length limit
- reply receipt record

### M4c: Durable Feishu Runtime

Goal:

```text
inbox/runs/outbox -> queue -> idempotent reply -> restart recovery
```

Scope:

- minimal SQLite or equivalent durable store
- inbox idempotency
- outbox receipt tracking
- pending run recovery

### M5: Context Modules

Scope:

- `system/root.md`
- `system/runtime.md`
- `agents/main/AGENT.md`
- basic context contributor shape
- context snapshots with source refs and hashes

Skill remains a plain context module until repeated use proves a real registry is
needed.

### M6: Invocation Gateway RFC Implementation

Scope:

- rename and harden tool execution around intent -> policy -> adapter -> receipt
- run principal
- per-channel execution profiles
- final system guard for approval resume

### M7: Plugin Registries

Scope:

- context contributor registry
- trusted hook registry
- external system manifests
- out-of-process adapters

### M8: Multi-Agent and Workflow

Scope:

- separate agent directories
- delegation packets
- external workflow source of truth
- command/query/event/receipt integration

### M9: Bounded Self-Evolution

Scope:

- git worktree candidate
- selected historical run replay
- evaluator script producing `score.json` and `report.md`
- promote through PR merge
- rollback to last-known-good tag

## Near-Term Rule

Do not add general hook runtime, skill runtime, external system registry, or heavy
sandbox before M4 and M5 prove the repeated shapes.
