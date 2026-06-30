# M5 Outbox Stabilization — Handover

This document hands off the M5 outbox stabilization work to a successor agent.
It assumes the new agent has no prior context and must execute from cold start.

Read every section before touching code. Violating the safety invariants or
the forbidden scope will undo the guarantees the current branch is built on.

## 0. Repo facts

- Workspace: `{home}/workspace/project/agent-core`
- Repo: `https://github.com/mayf3/agent-core.git`
- Default branch: `main` (HEAD `ed97f74` = merge of PR #38 feat/outbox-dispatch-once)
- Working branch pushed by previous agent: `feat/m5-outbox-stabilization`
  - `origin/feat/m5-outbox-stabilization` exists and matches local HEAD
  - Two commits on top of `main`:
    1. `ae24327 feat(m5): stabilize outbox dispatch lifecycle`
    2. `f9bc63f docs(m5): add stabilization plan, todo, layout validation`
- **No GitHub PR has been opened yet.** First task for the successor is to
  open the PR (see §3).

## 1. What was just shipped (do not redo)

The current branch delivers M5 outbox stabilization on top of the existing
M5 foundation. Headline changes (all in commit `ae24327`):

### Code

- `src/journal/outbox_queue.rs`
  - `succeed_outbox_dispatch()` / `fail_outbox_dispatch()` /
    `unknown_outbox_dispatch()` now require `status = 'dispatching'` and
    bail with `outbox_dispatch_terminal_transition_not_allowed` otherwise.
    No Journal event is written on rejection.
  - Success path writes `RunCompleted` + `UPDATE runs SET status='Completed'`
    in the same `BEGIN IMMEDIATE` transaction as `ReceiptReceived`.
  - Failure path writes `RunFailed` + `UPDATE runs SET status='Failed'` in
    the same tx as `ReceiptReceived(status=Failed)`.
  - Unknown path leaves Run in `WaitingDispatch` (intentional).
- `src/journal/unknown.rs`
  - `recover_unknown_invocations()` rewritten to append
    `OutboxDispatchUnknown` as the terminal fact. It no longer writes
    `ReceiptReceived(status=Unknown)`, no longer calls `fail_run`, no
    longer appends `RunCompleted`. Recovery never calls the adapter.
  - New `stale_dispatching_with_terminal_journal()` repairs projection rows
    that are stuck in `dispatching` while the Journal already has
    `OutboxDispatchUnknown` or `ReceiptReceived`. Repair is UPDATE-only,
    no duplicate Journal event.
- `src/domain/mod.rs` + `src/journal/sqlite.rs`
  - New `JournalEventKind::RunFailed` variant and matching `parse_kind`
    branch.
  - New public `JournalStore::run_status(run_id) -> Result<Option<String>>`.
- `src/journal/queue_health.rs`
  - New `outbox_status_count(status) -> Result<i64>` helper.
- `src/server/mod.rs`
  - `health_snapshot(journal, outbox_dispatcher_enabled)` signature now
    takes the dispatcher flag. JSON adds `outbox_dispatcher_enabled`,
    `outbox_pending_count`, `outbox_unknown_count`, `outbox_dispatching_count`.
  - `serve()` prints startup log:
    `outbox_dispatcher_enabled`, `existing_pending_outbox_count`,
    `existing_unknown_outbox_count`, `existing_dispatching_outbox_count`,
    `dispatcher will process pending/retryable outbox items`,
    `unknown items will not be retried automatically`.
- `src/journal/test_helpers.rs` (new public module)
  - `tamper_first_event_for_test`
  - `expire_outbox_lease_for_test`
  - `set_outbox_available_at_past_for_test`
  - `set_outbox_status_for_test`
  - Note: see follow-up §4.1 — these need to be tightened so production
    code cannot accidentally call them.
- `tests/common/mod.rs` — shared test helpers (`test_config`,
  `test_session`, `test_run`, `cli_principal`, `approved_stdout_invocation`,
  `runtime_run`). Marked `#![allow(dead_code)]` because each test binary
  uses a different subset.

### Documentation

- `docs/architecture/outbox.md` rewritten (single source, 330 lines):
  - "Current dispatch path" section clarifies dispatcher loop is wired in
    and `lease_next_outbox_dispatch` (not `start_outbox_dispatch`) is the
    dispatcher path.
  - "Dispatch outcome -> Run / Journal / Projection" matrix.
  - "`retryable_failed` is auto-retried (PR3 semantics)" section.
  - "Terminal transition guard" section documenting the new
    `dispatching`-only rule and how recovery sidesteps it.
  - "`unknown` detection (startup recovery)" section describing the two
    recovery cases (Task 4.1 and 4.2).
  - Health snapshot field reference.
- `docs/m5-outbox-stabilization/plan.md` — design rationale
- `docs/m5-outbox-stabilization/todo.md` — stage-by-stage execution log
- `docs/m5-outbox-stabilization/validation_layout.py` — layout anchor
  assertions, used as a regression net during the work

### Tests (65 total, all green)

- `tests/m5_dispatch_outcome.rs` — success completes run, definite failure
  fails run, unknown does not complete run, RunCompleted written exactly once
- `tests/m5_outbox_recovery.rs` — recovery writes `OutboxDispatchUnknown`
  (not ReceiptReceived), stale dispatching projection repair,
  `existing OutboxDispatchUnknown stops scan`, never returns to pending,
  health fields expose dispatcher state
- `tests/m5_outbox_retry.rs` — retryable_failed + available_at<=now is
  redispatched; future available_at is not; terminal/in-flight states
  (failed/dead/dispatching/unknown/succeeded) are not leased; terminal
  transition guard rejects non-dispatching state
- `tests/m5_queue_projection.rs` — outbox/worker projection lifecycle,
  idempotency, lease semantics, health projection counts
- `src/server/delivery.rs` inline tests — dispatcher drains multiple
  pending rows, dispatcher disabled does not drain, shutdown signal stops
  the loop, unknown outbox is skipped
- `tests/m0_kernel.rs` — vertical slice, recovery assertions, health

## 2. Safety invariants (must hold after any change)

These guarantees were verified by tests on the current branch. Any
follow-up work that breaks them is by definition a regression.

1. **No double delivery via automatic retry.** Once `DispatchStarted` is
   in the Journal, the row may only reach `succeeded` / `failed` /
   `unknown`. It may never return to `pending` / `retryable_failed`.
2. **`unknown` is terminal and silent.** No automatic redispatch. Only
   manual intervention or a future compensating process may handle these.
3. **`OutboxDispatchUnknown` is the terminal Journal fact for unknown
   dispatches.** Never fabricate `ReceiptReceived(status=Unknown)` for
   them.
4. **Terminal transition helpers require `status='dispatching'.** A row
   in any other projection state cannot be moved to `succeeded` /
   `failed` / `unknown` via the helper API.
5. **Run completion is bound to dispatch outcome.** Succeeded dispatch →
   `RunCompleted` + `runs.status='Completed'`. Definite failure →
   `RunFailed` + `runs.status='Failed'`. Unknown → Run stays
   `WaitingDispatch`.
6. **Runtime never calls the adapter directly.** It only enqueues to
   `outbox_dispatches`. The dispatcher loop is the only consumer.
7. **Journal is source of truth.** Projections are operational queues.
   If projection conflicts with Journal, Journal wins. Projections can
   be rebuilt later (not implemented yet — see §4.4).
8. **`ReceiptReceived` is only written on Succeeded and definite-Failed
   paths.** Never on Unknown.

## 3. Immediate next step: open the PR

The branch is pushed but no PR exists. Open one:

```bash
gh pr create \
  --base main \
  --head feat/m5-outbox-stabilization \
  --title "feat(m5): stabilize outbox dispatch lifecycle" \
  --body "..."
```

PR body should summarize:
- Run completion bound to dispatch outcome (success/failure/unknown)
- Terminal transition guard (`status='dispatching'` required)
- Recovery rewritten to `OutboxDispatchUnknown` terminal fact
- Stale `dispatching` projection repair (no duplicate Journal events)
- Dispatcher startup log + health fields
- Test coverage: 65 passing

Then watch CI. Required checks (per repo `pnpm check`):
- `node scripts/check-structure.mjs` (≤500 lines/file, ≤20 files/dir, ≤6 deep)
- `node scripts/check-local-secret-leaks.mjs`
- `cargo test`
- `pnpm check:connector` (TS connector tests)

If CI fails, do not force-push to fix; commit on top, push again.

## 4. Follow-up work (non-blocking, separate PRs)

The previous user message called out these items explicitly as future PRs.
Do NOT bundle them into the stabilization PR.

### 4.1 Tighten `src/journal/test_helpers.rs`

Current state: helpers are public on `JournalStore` with `_for_test`
suffixes as a naming convention. Nothing structurally prevents production
code from calling them.

Goal: enforce test-only access. Options:

- Move helpers behind `#[cfg(any(test, feature = "test-helpers"))]` and
  add a `test-helpers` Cargo feature that is only enabled by test builds.
- Or move them to a separate crate `agent-core-kernel-test` that
  re-implplements via public SQL.

The feature-flag approach is lower risk. Do not change helper behavior
while tightening.

### 4.2 PR6: Startup stale dispatching recovery

User spec verbatim:

> PR6 做 startup stale dispatching recovery，并让已有
> ReceiptReceived/OutboxDispatchUnknown 的 projection 按 Journal terminal
> fact 恢复为 succeeded/failed/unknown。unknown 绝不能自动重发。

Most of this already exists on the current branch (see
`recover_unknown_invocations` in `src/journal/unknown.rs`):

- `DispatchStarted` without `ReceiptReceived` and without
  `OutboxDispatchUnknown` → append `OutboxDispatchUnknown` + projection
  `unknown` (Task 4.1, done).
- Stale `dispatching` projection with terminal Journal event → projection
  repaired to `unknown` without duplicate event (Task 4.2, done).

What PR6 still needs to add:

- **Projection rebuild for rows whose Journal is terminal but projection
  is not.** Example: Journal has `ReceiptReceived(status=Succeeded)` but
  projection is still `dispatching` (or worse, `pending`). Recovery should
  reconcile projection to `succeeded`. Similarly for `Failed` → `failed`.
- Tests for each (Journal terminal × projection state) combination.
- **Crucially:** `unknown` reconciliation must never trigger adapter
  execution, must never enqueue a new dispatch, must never transition back
  to `pending` / `retryable_failed`.

Reuse the existing `stale_dispatching_with_terminal_journal()` query as
the starting point; extend it to inspect which terminal Journal event is
present and route to the matching projection state.

Branch name suggestion: `feat/m5-projection-reconcile`.

### 4.3 Adapter connect timeout (follow-up C)

`src/adapters/mod.rs` `HttpConnectorAdapter` uses `TcpStream::connect`
without a connect timeout. Only read/write timeouts are bounded. On
shutdown, the dispatcher thread's `join()` waits for the in-flight
`adapter.execute()` call; an unbounded connect could drag shutdown.

Goal: switch to `TcpStream::connect_timeout((addr, duration))` with a
duration aligned to the existing read/write timeout budget. Do not log
the underlying transport error verbatim; sanitize to a category string.

### 4.4 Dispatcher observability (follow-up D)

Current `/health` exposes:
- `outbox_dispatcher_enabled`
- `outbox_pending_count` / `outbox_unknown_count` / `outbox_dispatching_count`

Still missing:
- `outbox_dispatcher_running` — is the loop thread alive?
- `last_dispatch_tick_at` — when did the last poll cycle run?
- `last_dispatch_error_category` — short sanitized category of the last
  dispatcher error (timeout / connector_execute_failed / runtime_failed /
  ...), never the raw error string

Implementation sketch: hand an `Arc<AtomicBool>` running flag and an
`Arc<Mutex<Option<(DateTime<Utc>, String)>>>` last-tick/state into
`start_outbox_dispatcher_loop`. Loop sets running=true on entry, false on
exit (including panic via Drop guard). Each poll updates last-tick; each
error updates last-error. Health snapshot reads from these.

## 5. Forbidden scope (do not add)

These are out of scope for any M5 follow-up. Do not introduce them:

- Runtime synchronous send path (PR4 removed it; do not restore)
- Modifications to `connectors/feishu/` (TS connector is frozen)
- LLM / Context / Skill runtime changes
- Workflow / Multi-Agent / Shell / Memory
- Dynamic hooks / plugin registry / sandbox / self-evolution
- Reaction retry scheduling (separate track)
- Deploy / restart scripts
- Auto-resend of `unknown` outbox
- Transitioning `unknown` / `succeeded` / `failed` / `dead` / `dispatching`
  back to `pending` / `retryable_failed`

## 6. Hard environment rules

Never read, log, or commit:

- `.env`, `.env.*` (except `.env.example`)
- `~/.openduck`, `~/.openclaw`
- `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.crt`
- `secrets/`, `private/`
- `logs/`, `*.log`
- API keys, tokens, Authorization headers, tenant access tokens, app
  secrets, full HTTP headers, complete SDK error objects

`.gitignore` already excludes these. Before any commit, run:

```bash
git diff --cached --name-only | grep -E '\.(env|pem|key|p12|crt)$|^(secrets|private|logs)/' && echo "LEAK" && exit 1
```

Service restarts: only `com.agent-core.kernel` and
`com.agent-core.feishu-connector` may be restarted, and only if the user
authorizes. Never restart `openclaw` or any other service.

## 7. Verification protocol (run before any merge)

Every code change must pass:

```bash
cargo build
cargo test
pnpm check    # runs structure check + secret scan + cargo test + connector tests
git diff --check
python3 docs/m5-outbox-stabilization/validation_layout.py
```

`pnpm check` is the authoritative gate. It enforces:

- File length ≤ 500 lines
- Directory file count ≤ 20
- Directory depth ≤ 6
- No local secret leak patterns in diff
- Rust test suite
- TS connector test suite (6 tests)

If a file exceeds 500 lines, split it. Do not appeal to exceptions.

`validation_layout.py` asserts source/doc anchors stay in place. Update
the script whenever you intentionally move or rename an anchored symbol.

## 8. Code conventions

- All source comments and log messages in English. No Chinese in code.
- Match existing style: no trailing whitespace, LF line endings, 4-space
  indent in Rust.
- Newtypes for IDs (`RunId`, `SessionId`, `InvocationId`, ...) — never
  pass raw `String` for an ID.
- SQL strings use `rusqlite::params!` macro; status strings go through
  `WorkerJobStatus::as_str()` / `OutboxDispatchStatus::as_str()`, never
  inline string literals.
- Every Journal write goes through `append_event_tx` (transactional) or
  `append_event` (top-level). Both maintain the hash chain.
- Status transitions on projections must happen in the same transaction
  as the corresponding Journal append.

## 9. Key file map

```
src/domain/
  mod.rs              ID newtypes, core domain types, JournalEventKind enum
  status.rs           WorkerJobStatus / OutboxDispatchStatus enums
  retry.rs            RetryPolicy + next_retry_delay_ms
src/journal/
  mod.rs              module wiring
  sqlite.rs           JournalStore core, append_event, run/session/run helpers
  queue.rs            append_event_tx, queue schema migration
  queue_health.rs     status_counts, outbox_status_count
  outbox.rs           lease_next_outbox_dispatch (dispatcher loop entry)
  outbox_queue.rs     queue_outbox_dispatch, start/succeed/fail/unknown/mark_* helpers
  worker.rs           lease_next_worker_job, worker lifecycle helpers
  unknown.rs          unknown_invocations, recover_unknown_invocations,
                      stale_dispatching_with_terminal_journal
  recovery.rs         undelivered_ingress_events
  test_helpers.rs     _for_test helpers (tighten in §4.1)
  hash_chain.rs       event_hash
src/runtime/
  mod.rs              Runtime::deliver (enqueue-only, no sync send)
  outbox_dispatcher.rs  dispatch_once()
src/server/
  mod.rs              serve(), health_snapshot(), log_dispatcher_startup_state()
  delivery.rs         start_worker_loop, start_outbox_dispatcher_loop,
                      run_dispatcher, recover_undelivered_ingress
src/gateway/          ingress validation, invocation approval
src/adapters/         HttpConnectorAdapter, InvocationAdapter trait
src/llm/              OpenAI-compatible adapter
src/context.rs        ContextAssembler
src/config.rs         KernelConfig (env loading)
docs/architecture/outbox.md   canonical outbox semantics (read this first)
docs/m5-outbox-stabilization/ process notes for the stabilization PR
tests/common/mod.rs           shared test helpers
tests/m5_*.rs                  per-feature integration tests
```

## 10. Open questions for the human (not for the agent to decide)

These were raised during the work and not resolved. Surface them in the
PR description or in a follow-up conversation, do not guess:

- Should `RunStatus::Unknown` be introduced in a future phase to give
  unknown dispatches a non-`WaitingDispatch` Run state? Current decision
  is "no, Phase 0 uses `WaitingDispatch` + outbox projection + health".
- Should `parse_kind`'s `_ => JournalEventKind::RunCompleted` fallback
  (in `src/journal/sqlite.rs`) be tightened? It is a pre-existing footgun
  where any unrecognized kind silently becomes `RunCompleted`. Out of
  scope for the current PR but worth tracking.
  **Resolved:** the fallback now routes to a new `JournalEventKind::Unknown`
  sentinel instead of `RunCompleted` (see PR for §10). Unknown kinds no
  longer masquerade as a run completion in `undelivered_ingress_events`;
  `verify_hash_chain` still flags the row corrupt. `parse_kind`/`row_to_event`
  deliberately stay non-`Result` to preserve the existing `/health`
  `"corrupt"` semantics (bailing would 500 the endpoint instead).
- `RetryPolicy::lease_timeout_ms` (30s) vs the hardcoded 5-minute lease
  used inside `lease_next_worker_job` / `lease_next_outbox_dispatch`. Doc
  notes the divergence; unification is a future PR.
- `design-doc.html` is stale (describes JSONL-era architecture). Tagged
  for cleanup but intentionally not modified in this PR.

## 11. Final reminders

- Verify before claiming. Run `cargo test` and `pnpm check` after every
  change. State command output, not assumptions.
- Push-back is welcome when feedback is technically wrong. Cite tests or
  code paths. No performative agreement.
- One PR per concern. Do not bundle §4.1 / §4.2 / §4.3 / §4.4 into a
  single PR.
- If the user authorizes a service restart for verification, only restart
  `com.agent-core.kernel` or `com.agent-core.feishu-connector`. Never
  restart `openclaw`.

— end of handover —
