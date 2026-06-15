# Worker Jobs and Outbox Dispatches

## Source of Truth vs Projection

| Layer | Role |
|---|---|
| **Journal** (`journal_events`) | **Single source of truth.** Append-only, hash-chained event log. All state transitions are recorded as Journal events. |
| **worker_jobs** | **Rebuildable projection / operational queue.** Derived from Journal. Tracks lease state for worker delivery. |
| **outbox_dispatches** | **Rebuildable projection / operational queue.** Derived from Journal. Tracks lease state for outbox dispatch. |

**Invariant**: If a projection value conflicts with the Journal, the Journal wins.

Projections can be repaired or rebuilt from the Journal at any time. They are not workflow state and not Agent-visible state.

---

## Current dispatch path

The Runtime no longer sends approved invocations synchronously. After
`InvocationApproved` it only calls `queue_outbox_dispatch`, transitions the
`Run` to `WaitingDispatch`, and returns. A single in-process outbox dispatcher
loop (`start_outbox_dispatcher_loop`) is wired into `serve()` and polls
`outbox_dispatches` via `dispatch_once()`.

```text
Runtime.deliver
  -> queue_outbox_dispatch (OutboxQueued, projection=pending)
  -> Run.status = WaitingDispatch
outbox dispatcher loop
  -> dispatch_once
     -> lease_next_outbox_dispatch (DispatchStarted, projection=dispatching)
     -> adapter.execute
     -> succeed_outbox_dispatch | fail_outbox_dispatch | unknown_outbox_dispatch
```

`lease_next_outbox_dispatch()` performs both the projection transition
(`pending` or `retryable_failed` → `dispatching`) and the `DispatchStarted`
journal append inside the same transaction. `dispatch_once()` never calls
`start_outbox_dispatch()`.

`start_outbox_dispatch()` (`src/journal/outbox_queue.rs`) is a separate helper
that accepts only `pending → dispatching` and is used by tests to drive the
projection manually. It is **not** on the dispatcher loop path.

`outbox_dispatcher_enabled` defaults to `true`. The kernel prints startup lines
that expose `existing_pending_outbox_count`, `existing_unknown_outbox_count`,
and `existing_dispatching_outbox_count` so the operator can see what the loop
will pick up. Unknown rows are never retried automatically.

When `outbox_dispatcher_enabled=true`, existing pending/retryable_due outbox
rows may be dispatched on startup. This is expected because `pending` means
`DispatchStarted` has not been recorded. Unknown outbox rows are never retried
automatically; only manual intervention or a future compensating process may
handle them.

---

## Dispatch outcome -> Run / Journal / Projection

| Outcome | Journal events (in one tx) | `outbox_dispatches.status` | `runs.status` |
|---|---|---|---|
| **Succeeded** | `ReceiptReceived(status=Succeeded)` + `RunCompleted(status=Completed)` | `succeeded` | `Completed` |
| **Definite failed** (`ReceiptStatus::Failed`) | `ReceiptReceived(status=Failed)` + `RunFailed(status=Failed)` | `failed` | `Failed` |
| **Unknown** (adapter `Err`, timeout, transport failure) | `OutboxDispatchUnknown` | `unknown` | unchanged (`WaitingDispatch`) |

Important: an **unknown outbox does not complete the Run**. The Run stays in
`WaitingDispatch` and the unknown projection row plus the `/health` field
`outbox_unknown_count` surface the situation for manual handling. The kernel
intentionally does not introduce a `RunStatus::Unknown` variant in Phase 0.

`ReceiptReceived` is only ever written on the Succeeded and definite-Failed
paths. Recovery never fabricates a `ReceiptReceived(status=Unknown)`.

---

## Worker Job Status

```text
queued
  → leased (future)
    → running
      → succeeded
      → retryable_failed (future)
      → dead (future)
```

| Status | Meaning | Leasable |
|---|---|---|
| `queued` | Awaiting lease | Yes |
| `leased` | Leased by worker, not yet confirmed running | Yes (after lease timeout) |
| `running` | Actively being processed | Yes (after lease timeout) |
| `succeeded` | Completed successfully | No |
| `retryable_failed` | Failed, eligible for retry | Yes (when `available_at <= now`) |
| `dead` | Terminal failure, no further retries | No |

### Current Phase 0 statuses in use

Phase 0 uses a subset: `queued` → `running` → `succeeded` / `failed`. The `failed` status is transitional and will be replaced by `retryable_failed` / `dead` in future PRs. `leased` is not yet used; `queued` transitions directly to `running`.

---

## Outbox Dispatch Status

```text
pending
  → leased (future)
    → dispatching
      → succeeded
      → failed (definite business failure)
      → retryable_failed (eligible for re-lease when available_at<=now)
      → unknown (no receipt, no auto-retry)
      → dead (future)
```

| Status | Meaning | Leasable |
|---|---|---|
| `pending` | Awaiting lease | Yes |
| `leased` | Leased by dispatcher, not yet confirmed dispatching | Yes (after lease timeout) |
| `dispatching` | In flight, external call initiated | No (until lease timeout) |
| `succeeded` | External side effect confirmed via Receipt | No |
| `failed` | Definite business failure (e.g. argument error) | No |
| `retryable_failed` | Failed before DispatchStarted, eligible for retry | Yes (when `available_at <= now`) |
| `unknown` | DispatchStarted occurred but no ReceiptReceived. **Do not auto-retry.** Must be handled manually or via compensating path. | No |
| `dead` | Terminal failure, no further retries | No |

### Critical invariant: `DispatchStarted` without `ReceiptReceived`

Once `DispatchStarted` is appended to the Journal:

- The dispatcher MUST NOT automatically retry.
- The outbox MUST be marked `unknown`.
- Recovery appends `OutboxDispatchUnknown` as the terminal fact. It does not
  fabricate `ReceiptReceived(status=Unknown)`.
- Only manual intervention or a future compensating process may handle these.

This is the core safety guarantee: **no double delivery** via automatic retry.

### `retryable_failed` is auto-retried (PR3 semantics)

`lease_next_outbox_dispatch()` selects both `pending` rows and `retryable_failed`
rows whose `available_at <= now`, transitions them to `dispatching`, and appends
`DispatchStarted` in the same transaction. A `retryable_failed` row whose
`available_at > now` stays parked until the next lease poll.

This is the path:
```text
retryable_failed + available_at <= now
  → lease_next_outbox_dispatch
  → dispatching + DispatchStarted
  → adapter.execute
  → succeed_outbox_dispatch | fail_outbox_dispatch | unknown_outbox_dispatch
```

### `unknown` detection (startup recovery)

`recover_unknown_invocations()` runs once at kernel startup and handles two
cases:

1. **Journal-only candidates** (Task 4.1): `DispatchStarted` exists, no
   `ReceiptReceived`, no `OutboxDispatchUnknown`. For each candidate the
   recovery writes `OutboxDispatchUnknown` and updates the projection to
   `unknown` in the same transaction. It never calls the adapter.

2. **Stale `dispatching` projection with terminal journal** (Task 4.2):
   projection row is still `dispatching` with `locked_until <= now` while the
   journal already has `OutboxDispatchUnknown` or `ReceiptReceived`. This can
   happen if a previous recovery commit landed between the journal append and
   the projection update. The recovery reconciles the projection to match the
   terminal fact **without appending a duplicate journal event**:

   | Journal terminal fact | Projection target |
   |---|---|
   | `ReceiptReceived` with `status = Succeeded` | `succeeded` |
   | `ReceiptReceived` with `status = Failed` | `failed` |
   | `OutboxDispatchUnknown` (or a receipt whose status cannot be parsed) | `unknown` |

   The classifier never promotes an ambiguous receipt to `succeeded`; any
   unrecognized status defaults to `unknown`.

Recovery never:
- calls `adapter.execute()` or `dispatch_once()`
- writes `ReceiptReceived`
- writes `RunCompleted` or `RunFailed`
- transitions a row back to `pending` or `retryable_failed`

---

## Lease semantics

`lease_next_worker_job()` selects:

- `status = queued`
- `status = running` with expired `locked_until` (stale lease recovery)
- (future) `status = retryable_failed` with `available_at <= now`

`lease_next_outbox_dispatch()` selects:

- `status = pending`
- `status = retryable_failed` with `available_at <= now`

Both functions **must not** select:

- `succeeded`
- `dead`
- `unknown` (outbox only)
- `failed` (outbox only — definite business failure)
- `dispatching` (outbox only — unless lease expired)

### Leasable states

| Table | Leasable | Not leasable |
|---|---|---|
| `worker_jobs` | `queued`, `retryable_failed` + `available_at <= now`, stale `running` | `leased`, `running` (active), `succeeded`, `failed`, `dead` |
| `outbox_dispatches` | `pending`, `retryable_failed` + `available_at <= now` | `leased`, `dispatching`, `succeeded`, `failed`, `unknown`, `dead` |

### Stale lease recovery

- Stale `dispatching` (outbox) → **reconciled to match the Journal terminal
  fact** (`succeeded` / `failed` / `unknown` per the matrix above), never
  `retryable_failed`. A stale `dispatching` row with no terminal Journal fact
  is handled by Task 4.1, which appends `OutboxDispatchUnknown` and sets the
  projection to `unknown`.
- Stale `running` (worker) → can become `retryable_failed` or `queued`.

---

## Dispatch outcome contract

`dispatch_once()` handles outcomes in `src/runtime/outbox_dispatcher.rs`:

| Outcome | Adapter result | Journal events | outbox status | run status |
|---|---|---|---|---|
| **Succeeded** | `Ok(Receipt{status: Succeeded})` | `ReceiptReceived` + `RunCompleted` | `succeeded` | `Completed` |
| **Definite failed** | `Ok(Receipt{status: Failed})` | `ReceiptReceived(status=Failed)` + `RunFailed` | `failed` | `Failed` |
| **Unknown after dispatch** | `Err(e)` or `Ok(Receipt{status: Unknown})` | `OutboxDispatchUnknown` | `unknown` | unchanged |

### Safety rule: DispatchStarted before adapter call

`lease_next_outbox_dispatch()` writes `DispatchStarted` to the Journal **before** calling the adapter. This means:

- If `adapter.execute()` returns `Err(...)` (timeout, transport failure), the Journal already has `DispatchStarted` → **must go to `unknown`**, never auto-retry.
- If `adapter.execute()` returns `Ok(Receipt{status: Failed})` (definite business failure), `ReceiptReceived(status=Failed)` + `RunFailed` are written and the Run is marked `Failed`.
- If `adapter.execute()` returns `Ok(Receipt{status: Succeeded})`, `ReceiptReceived` + `RunCompleted` are written and the Run is marked `Completed`.

### Terminal transition guard

`succeed_outbox_dispatch()` / `fail_outbox_dispatch()` / `unknown_outbox_dispatch()`
accept **only** `status = dispatching` rows. Any other projection state causes
the helper to bail with `outbox_dispatch_terminal_transition_not_allowed` and
no Journal event is written. This prevents a future caller from silently
overwriting a terminal state with a different terminal fact.

Recovery paths do not go through these helpers:

- `recover_unknown_invocations()` step A appends `OutboxDispatchUnknown` and
  updates the projection directly via SQL (candidates are journal-only
  DispatchStarted rows whose projection may be in any non-terminal state).
- step B uses an explicit `status = 'dispatching'` WHERE clause so it only
  repairs stale `dispatching` rows whose journal is already terminal. The
  projection target is derived from the terminal fact (`ReceiptReceived`
  status → `succeeded` / `failed`, `OutboxDispatchUnknown` → `unknown`).

### Pre-DispatchStarted failures

If a failure occurs before `lease_next_outbox_dispatch()` (e.g. reconstructing `ApprovedInvocation` fails), the outbox **never left `pending`** and can safely transition to `retryable_failed` or `dead`. This is handled by the caller, not by `dispatch_once()`.

### Journal as source of truth for dispatch

The Journal is the sole authority on whether a dispatch actually started:

| Journal has | Meaning |
|---|---|
| `DispatchStarted` + `ReceiptReceived(status=Succeeded)` + `RunCompleted` | Definitely sent, Run completed |
| `DispatchStarted` + `ReceiptReceived(status=Failed)` + `RunFailed` | Definitely failed, Run failed, no external side effect |
| `DispatchStarted` + `OutboxDispatchUnknown` | Unknown; must not auto-retry, Run not completed |
| `DispatchStarted` only (no Receipt, no OutboxDispatchUnknown) | Unknown at runtime; recovery marks `unknown` and appends `OutboxDispatchUnknown` |
| No `DispatchStarted` | Never attempted; safe to retry |

---

## Retry policy

### Default constants (`RetryPolicy`)

| Parameter | Default | Description |
|---|---|---|
| `max_worker_attempts` | 3 | Max lease attempts before a worker job is marked `dead` |
| `max_outbox_attempts` | 3 | Max lease attempts before an outbox dispatch is marked `dead` |
| `base_retry_delay_ms` | 1_000 | Exponential backoff base (ms) |
| `max_retry_delay_ms` | 30_000 | Maximum backoff cap (ms) |
| `lease_timeout_ms` | 30_000 | How long a lease is valid before another worker can reclaim |

Note: `lease_timeout_ms` (30s) is the canonical value in `RetryPolicy`. Worker and outbox lease currently hardcode 5-minute timeouts internally. These will be unified to use `RetryPolicy.lease_timeout_ms` in a future PR.

### `attempts` semantics

- `attempts` is incremented **each time a row is successfully leased**.
- `next_retry_delay_ms(attempts)` = `min(base * 2^(attempts-1), max)`.
- If `attempts >= max_attempts` on failure, the row goes to `dead` instead of `retryable_failed`.

### `available_at` semantics

- `available_at` controls when a `retryable_failed` row may be leased again.
- `available_at <= now` → leasable.
- `available_at > now` → not leasable (future retry not yet due).
- `pending` / `queued` rows always have `available_at <= created_at` so they are immediately leasable.

### Retry helpers

| Helper | Writes status | Journal event |
|---|---|---|
| `mark_worker_retryable_failed()` | `retryable_failed` + `available_at` | `WorkerJobFailed(retryable=true)` |
| `mark_worker_dead()` | `dead` | `WorkerJobDead` |
| `mark_outbox_retryable_failed()` | `retryable_failed` + `available_at` | `OutboxDispatchFailed(retryable=true)` |
| `mark_outbox_dead()` | `dead` | `OutboxDispatchDead` |

---

## Health snapshot fields

`GET /health` reports:

```json
{
  "ok": <bool>,
  "status": "ok" | "degraded" | "corrupt",
  "hash_chain_ok": <bool>,
  "journal_event_count": <i64>,
  "undelivered_ingress_count": <i64>,
  "worker_jobs": { "<status>": <count> },
  "outbox_dispatches": { "<status>": <count> },
  "outbox_dispatcher_enabled": <bool>,
  "outbox_pending_count": <i64>,
  "outbox_unknown_count": <i64>,
  "outbox_dispatching_count": <i64>,
  "outbox_dispatcher_running": <bool>,
  "last_dispatch_tick_at": "<rfc3339> | null",
  "last_dispatch_error_category": "<category> | null",
  "unknown_invocation_count": <i64>,
  "unknown_invocations": [ { "invocation_id": "...", "run_id": "...", "session_id": "...", "first_dispatch_at": "..." } ]
}
```

The three dispatcher-observability fields are backed by a shared
`DispatcherMetrics` handle (`src/server/dispatcher_metrics.rs`) handed to the
loop thread:

- `outbox_dispatcher_running` -- true while the loop thread is alive. Set on
  entry, cleared on exit via an RAII `LoopGuard` (so it is cleared even if the
  loop panics).
- `last_dispatch_tick_at` -- RFC3339 timestamp of the last completed poll
  cycle, or `null` before the first tick.
- `last_dispatch_error_category` -- sanitized category of the last loop-level
  error (e.g. `timeout`, `connector_execute_failed`, `runtime_failed`), or
  `null`. This tracks loop-level failures (when `dispatch_once` itself returns
  `Err`). Per-dispatch adapter failures are already captured per-row in
  `outbox_dispatches.last_error` via `unknown_outbox_dispatch`; the raw error
  string is never surfaced.

---

## Status value contract

All status values are defined in `src/domain/status.rs` as `WorkerJobStatus` and `OutboxDispatchStatus` enums with `as_str()` / `from_str()` helpers. SQL queries **must** use these helpers rather than hardcoded string literals.
