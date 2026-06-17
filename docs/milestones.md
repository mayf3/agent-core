# Agent Core Milestones

This file is the施工单. It deliberately excludes long-term protocol detail; see
[Architecture RFC](./architecture-rfc.md) for invariants and future contracts.
For the final product shape and macro roadmap, see
[Product Roadmap](./product-roadmap.md).

## Current Status

| Milestone | Status | Notes |
|---|---|---|
| Rust Phase 0 M0 | Done | Rust Kernel CLI vertical slice with SQLite Journal |
| Rust Phase 0 M1 | Done | TS Feishu long-connection connector + Rust Echo |
| Rust Phase 0 M1a | Done | Connector-local reaction state survives connector restart |
| Rust Phase 0 M1b | Done | Connector-local reaction state JSONL is compacted |
| Rust Phase 0 M2 | Done | Feishu text now uses Rust OpenAI-compatible LLM path |
| Rust Phase 0 M3a | Done | health probe, unknown scan, disabled spawn/yield ABI |
| Rust Phase 0 M3b | Done | file-backed context, skill catalog, recent messages, truncation |
| Rust Phase 0 M3c | Done | startup recovery marks unknown dispatches without mutating history |
| Rust Phase 0 M3d | Done | SIGINT/SIGTERM stops accepting new kernel connections gracefully |
| Cleanup | Done | legacy Node agent runtime packages removed |
| Rust Phase 0 M4a | Done | `/v1/ingress` returns accepted before background delivery finishes |
| Rust Phase 0 M4b | Done | restart requeues reconstructable accepted ingress events |
| Rust Phase 0 M4c | Done | graceful shutdown drains started background delivery threads |
| Rust Phase 0 M4d | Done | health reports undelivered ingress backlog count |
| Rust Phase 0 M5a | Done | worker/outbox projection tables and idempotent queue methods |
| Rust Phase 0 M5b | Done | accepted ingress records worker job lifecycle in projection |
| Rust Phase 0 M5c | Done | current dispatch path records outbox lifecycle in projection |
| Rust Phase 0 M5d | Done | health reports worker/outbox projection status counts |
| Rust Phase 0 M5e | Done | single worker loop leases queued ingress jobs |
| Rust Phase 0 M5f | Done | worker job leases use timeout locks |
| Rust Phase 0 M5g | Done | unknown dispatch recovery updates outbox projection and blocks auto resend |
| Rust Phase 0 M5h | Done | stale running worker job crash-test coverage |
| Rust Phase 0 M5i | Done | `JournalStore::lease_next_outbox_dispatch()` leases pending outbox rows with lock fields and appends `DispatchStarted` |
| Rust Phase 0 M5j | Done | outbox projection stores approval decision IDs for future dispatcher use |
| Rust Phase 0 M5+ | Done | dispatch_once loop wired into server startup; Runtime delegates to outbox; connector reaction retry scheduling |
| Phase 1 H1 | Done | Journal kind decode tightened (`parse_kind` -> `Unknown` sentinel); unrecognized kinds no longer masquerade as `RunCompleted` |
| Phase 1 H2 | Done | migration check via `PRAGMA user_version` at startup; newer-than-supported schema rejected cleanly |
| Phase 1 H3 | Done | parse/kind drift detection (`row_to_event` sanitized eprintln on unrecognized kind) |
| Phase 1 H4 | Done | dispatcher observability in `/health` (`outbox_dispatcher_running`, `last_dispatch_tick_at`, `last_dispatch_error_category`) |
| Phase 1 H5 | Done | `/health` exposes stale dispatching lease count (`outbox_stale_dispatching_count`) and projection drift count (`outbox_projection_drift_count`) |
| Phase 1 H6 | Done | release checklist (`docs/release-checklist.md`) + operating guide (`docs/operating-guide.md`) |
| Phase 1 H7 | Done | restart-recovery lifecycle end-to-end test (`tests/m1_restart_recovery_lifecycle.rs`); recovery verified idempotent + never auto-retries |
| Phase 1 D1 | Done | `RunStatus::Unknown` introduced: recovery advances `runs.status` to `"Unknown"` when an outbox row is reconciled to unknown (distinct from `WaitingDispatch`); analysis in `docs/decisions/runstatus-unknown.md`, regression in `tests/m1_runstatus_unknown.rs` |
| Phase 2 M2a | Done | operation catalog as single source of truth (`src/domain/operation.rs`); gateway allowlist + runtime intent + adapter test now reference catalog constants; `HttpConnectorAdapter` reads the connector's reported `receipt.status` instead of assuming 2xx ⇒ Succeeded. **Typed-error follow-up (PR #75):** `AdapterError` thiserror enum in `src/domain/mod.rs` (`Timeout`/`ExecuteFailed`/`MalformedResponse`/`InvalidArgument`/`Transport`) replaces the fragile string-substring `from_error` sniffing; `from_error` now downcasts variant→category. Scoping in `docs/decisions/phase2-invocation-gateway-scoping.md` |
| Phase 2 M2b | Done | run principal `ExecutionProfile` introduced (`src/domain/operation.rs`): the four inline `CapabilityGrant` constructions across the gateway ingress/recovery paths now derive from `ExecutionProfile::for_channel(channel)`. Grants are **config-driven**: a new `KernelConfig.extra_allowed_operations` (env `AGENT_CORE_EXTRA_ALLOWED_OPERATIONS`) augments the baseline profile via `with_extra`; unknown/non-catalog names are dropped, and the default (empty) is identical to the previous inline literals. Exit criterion met: cli/feishu grant set is configurable. Tests in `src/domain/operation.rs` + `tests/m2b_config_grants.rs`. |
| Phase 2 M2c | Done | fixed invocation policy pipeline (`src/gateway/policy.rs`): `approve_invocation`'s inline 3-clause access ladder (grant → catalog → session-scope) lifted into a pure `evaluate_policy(intent, run, session) -> PolicyVerdict::{Allow, Deny(reason)}` function, no I/O, no `Gateway` state. First denial wins; error messages preserved (`capability_not_enabled` / `operation_not_allowed` / `target_session_mismatch`). Argument-shape validation stays in `approve_invocation` (schema concern, deferred to `argument_schema`). 6 isolated policy tests added. |

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

### Done: Phase 0 M5 Minimal Durable Worker / Outbox

Built the smallest durable async runtime slice without adding workflow
semantics. Status: complete.

Done:

- `worker_jobs` and `outbox_dispatches` projection tables;
- idempotent queue methods that append Journal facts and update projections in
  one transaction;
- accepted ingress queues `worker_jobs`, and current delivery threads update
  worker job started/succeeded/failed status;
- `Runtime::deliver` queues an `outbox_dispatches` row
  (`JournalStore::queue_outbox_dispatch`) instead of sending synchronously,
  and updates the run to `WaitingDispatch`;
- `/health` reports worker/outbox status counts for manual testing;
- `/v1/ingress` returns after queueing and a single in-process worker loop
  leases queued `worker_jobs`;
- worker job leases set `locked_by`/`locked_until` and can reclaim expired
  running jobs.
- startup unknown recovery marks dispatches without receipts as `unknown` in
  `outbox_dispatches` and does not auto-resend them.
- `JournalStore::lease_next_outbox_dispatch()` can lease one pending outbox
  row, set lock fields, and append `DispatchStarted` in one transaction.
- outbox projection rows carry the original approval `decision_id` for future
  dispatcher calls.
- `dispatch_once()` helper in `src/runtime/outbox_dispatcher.rs` leases one pending outbox row, executes it through the adapter, and marks it succeeded.
- the `dispatch_once` loop is wired into server startup
  (`start_outbox_dispatcher_loop` in `src/server/delivery.rs`, called from
  `serve()`); connector-local reaction retry scheduling is implemented via a
  bounded `withRetry` helper in `connectors/feishu/src/reactions.ts`.

Remaining:

- _(none -- M5 is complete.)_

### Later: Invocation Gateway Hardening

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
