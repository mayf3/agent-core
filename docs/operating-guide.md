# Operating Guide

Phase 1 Operational Hardening. This is the operator runbook for running,
diagnosing, and recovering an Agent Core deployment. It covers the surfaces
that actually exist on `main` — no speculative tooling.

## Architecture (what talks to what)

```text
Feishu / CLI
  -> TS Feishu Connector (edge adapter, separate process)
     -> POST /v1/ingress  (Rust Kernel)
        -> worker_jobs (projection) -> Runtime -> Context -> LLM
        -> outbox_dispatches (projection) -> dispatcher loop -> adapter -> receipt
        -> Journal (SQLite, append-only, hash-chained) = source of truth
```

- The **Rust Kernel** is the only Runtime / Gateway / Journal. It owns run
  lifecycle, session identity, approval, dispatch, and recovery.
- The **TS Feishu Connector** is an edge adapter. It translates Feishu events
  into kernel `/v1/ingress` calls and renders replies + reactions. It does not
  own Runtime, Gateway, or Journal state.
- `worker_jobs` and `outbox_dispatches` are **projections** (operational
  queues). The **Journal** (`journal_events` table, hash-chained) is the source
  of truth; projections can be reconciled from it.

## Starting the services

### Kernel

```bash
cargo run -- <db_path>
```

Defaults: kernel listens on `127.0.0.1:$AGENT_CORE_KERNEL_PORT` (default
`4130`). Runtime data lives under `$AGENT_CORE_DATA_DIR` (default
`~/.agent-core`).

### Feishu Connector

```bash
cd connectors/feishu
pnpm install
# set env vars (see below), then:
pnpm start
```

Listens on `$AGENT_CORE_CONNECTOR_PORT` (default `4131`).

## Required environment variables

Secrets are **never** committed. Load them from the environment at runtime.

**Kernel:** `AGENT_CORE_IPC_TOKEN`, `AGENT_CORE_OPENAI_API_KEY`,
`AGENT_CORE_OPENAI_BASE_URL`, `AGENT_CORE_MODEL`,
`AGENT_CORE_CONNECTOR_EXECUTE_URL`,
`AGENT_CORE_FEISHU_ALLOWED_OPEN_IDS` / `_CHAT_IDS`,
`AGENT_CORE_FEISHU_REQUIRE_GROUP_MENTION`,
`AGENT_CORE_OUTBOX_DISPATCHER_ENABLED` / `_POLL_MS`,
`AGENT_CORE_CONTEXT_RECENT_MESSAGES` / `_MAX_BLOCK_CHARS`,
`AGENT_CORE_MODEL_TIMEOUT_MS`.
Optional fallback model: `AGENT_CORE_FALLBACK_*`.

**Connector:** `AGENT_CORE_FEISHU_APP_ID`, `AGENT_CORE_FEISHU_APP_SECRET`,
`AGENT_CORE_IPC_TOKEN` (must match the kernel),
`AGENT_CORE_KERNEL_INGRESS_TIMEOUT_MS`,
`AGENT_CORE_FEISHU_PROCESSING_REACTION` / `_FAILED_REACTION`,
`AGENT_CORE_FEISHU_REACTION_RETRY_ATTEMPTS` / `_BASE_DELAY_MS`,
`AGENT_CORE_FEISHU_REACTION_STATE_PATH`.

Never put these in the repo. Never log tokens/keys.

## Reading system state: `/health`

```bash
curl http://127.0.0.1:4130/health | jq .
```

Key fields:

| Field | Meaning | Healthy |
|---|---|---|
| `status` | rollup | `ok` |
| `hash_chain_ok` | Journal integrity | `true` |
| `journal_event_count` | total events | grows monotonically |
| `undelivered_ingress_count` | ingress not yet turned into a worker job | `0` at steady state |
| `outbox_dispatcher_enabled` | dispatcher loop configured | `true` in production |
| `outbox_dispatcher_running` | loop thread alive | `true` |
| `last_dispatch_tick_at` | last poll cycle (RFC3339) | recent |
| `outbox_pending_count` | queued, not yet dispatched | may be >0 briefly |
| `outbox_dispatching_count` | in-flight | may be >0 briefly |
| `outbox_stale_dispatching_count` | inline-leased dispatches with expired lease (crash-abandoned) | `0` |
| `outbox_unknown_count` | dispatched but terminal outcome unknown | `0` at steady state |
| `outbox_projection_drift_count` | projection status disagrees with Journal terminal fact | `0` at steady state |
| `worker_job_stale_count` | worker jobs flagged `running` with expired lease (crash mid-job) | `0` |
| `unknown_invocation_count` / `unknown_invocations` | runs stuck in unknown state | `0` at steady state |

`status` values:
- `ok` — hash chain intact, no live unknown invocations, no terminal-unknown
  outbox rows, and no projection drift.
- `degraded` — hash chain intact but the Kernel's state is not fully
  trustworthy: live unknown invocations present (dispatch started, no terminal
  receipt), terminal-unknown outbox rows (recovered, outcome permanently
  undetermined), or projection drift (projection disagrees with the Journal
  terminal fact). Self-healing stale counts (`outbox_stale_dispatching_count`,
  `worker_job_stale_count`) do **not** degrade status — they are cleared by
  the next lease reclaim. See `docs/decisions/health-rollup-semantics.md`.
- `corrupt` — hash chain broken (Journal tampering or disk corruption;
  investigate immediately).

## Recovery behavior (automatic, on startup)

On startup the kernel reconciles projections to the Journal:

1. **Undelivered ingress** — ingress events with no `SessionReady`/`RunStarted`/
   `RunCompleted` correlation are re-enqueued as worker jobs.
2. **Stale dispatching with a terminal Journal fact** — a `dispatching` row
   whose Journal already has `ReceiptReceived` / `OutboxDispatchUnknown` is
   reconciled to match the fact (`succeeded` / `failed` / `unknown`). No
   duplicate Journal events, no adapter calls.
3. **Unknown invocations** — `DispatchStarted` with no terminal fact becomes
   `OutboxDispatchUnknown` (appended as a terminal fact) and the projection is
   set to `unknown`. **Never auto-retried.**

Invariants that always hold:
- The kernel **never automatically redispatches** an unknown or stale row.
- The Journal is append-only and hash-chained; tampering is detectable.
- Terminal transitions (`succeed`/`fail`/`unknown_outbox_dispatch`) are guarded:
  they reject any row not in `status = 'dispatching'`.

## Common faults and operator actions

### `status: corrupt`

The Journal hash chain is broken. This means tampering or disk corruption.
**Do not** attempt to "fix" it by editing the DB. Preserve the DB for
forensics, restore from a known-good backup, and restart. Investigate how the
tampering occurred.

### `outbox_unknown_count > 0` after a crash

Expected after an unclean shutdown. Recovery on the next startup marks these
`unknown` (terminal). They are **not** retried. If you need to re-send, that is
a deliberate human decision (out of scope for automatic recovery); inspect the
Journal to determine the true outcome before acting.

### `outbox_stale_dispatching_count > 0`

An inline-leased dispatch was abandoned (crash after
`lease_next_outbox_dispatch`). Recovery reconciles it on the next startup. If
it persists across restarts, the Journal terminal fact is missing — treat as
`unknown`.

### `outbox_projection_drift_count > 0`

A projection row's status disagrees with the Journal terminal fact (e.g.
projection says `dispatching` but the Journal has `ReceiptReceived(Succeeded)`).
At steady state this is 0 because startup recovery reconciles drift. If it
persists, recovery did not run or a race left the projection inconsistent —
restart the kernel to force reconciliation; if it still persists, inspect the
Journal for the invocation to determine the true terminal fact.

### `worker_job_stale_count > 0`

A worker job flagged `running` has an expired lease — the worker loop crashed
mid-job and never released the lease. On the next startup the worker loop
re-leases stale jobs (`lease_next_worker_job` treats `running` with expired
lease as re-leasable), so this should self-heal on restart. If it persists,
inspect `worker_jobs.last_error` for the failing job.

### Connector not receiving Feishu events

1. Confirm the connector process is running and listening on
   `$AGENT_CORE_CONNECTOR_PORT`.
2. Confirm `AGENT_CORE_IPC_TOKEN` matches between kernel and connector.
3. Confirm the kernel `/health` is `ok` and `undelivered_ingress_count` is not
   climbing (which would indicate the worker loop is stuck).
4. Check connector logs for `reaction retry scheduled` (transient Feishu API
   errors are retried with bounded backoff; persistent failures exhaust after
   `AGENT_CORE_FEISHU_REACTION_RETRY_ATTEMPTS`).

### Duplicate Feishu replies

The kernel is conservative about duplicates. If you observe duplicates, check:
- `journal_events` for repeated `ReceiptReceived` with the same
  `correlation_id` (indicates a replay bug).
- The connector's `reaction_state` JSONL for stranded `processing` states
  (restart the connector to drive cleanup via `markSucceeded`/`markFailed`).

## Schema version

The kernel stamps `PRAGMA user_version` on the DB. If a DB written by a newer
kernel is opened by an older binary, startup fails with:

```
database schema version N is newer than supported version 1; upgrade the kernel
```

This is intentional — it prevents an older kernel from corrupting a newer
schema. Upgrade the kernel binary.

## What the operator should never do

- Edit `journal_events`, `worker_jobs`, or `outbox_dispatches` rows directly
  (the Journal is hash-chained; projections are reconciled automatically).
- Manually retry an `unknown` dispatch without first determining the true
  outcome from the Journal.
- Commit or log `.env`, API keys, tokens, or `~/.agent-core` / `~/.openduck` /
  `~/.openclaw` paths.
- Run the Feishu connector as anything other than an edge adapter (it must not
  own Runtime / Gateway / Journal state).
