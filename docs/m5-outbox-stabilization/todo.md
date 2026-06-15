# M5 Outbox Stabilization — TODO

Stage 0 — Scaffolding
- [x] 0.1 Create branch `feat/m5-outbox-stabilization` from PR5 dirty tree
- [x] 0.2 Read docs/ and src/ to confirm current reality
- [x] 0.3 Write `plan.md`
- [x] 0.4 Write `validation_layout.py`
- [x] 0.5 Write `todo.md`

Stage 1 — Validation net first
- [x] 1.1 Run `validation_layout.py` to baseline current file layout (failed as expected)

Stage 2 — Domain event kind
- [x] 2.1 Add `JournalEventKind::RunFailed` to `src/domain/mod.rs`
- [x] 2.2 Add `"RunFailed" => JournalEventKind::RunFailed` to `parse_kind` in `src/journal/sqlite.rs`
- [x] 2.3 Run `cargo build` to confirm variants compile

Stage 3 — Success path completes Run
- [x] 3.1 Modify `succeed_outbox_dispatch` to write `RunCompleted` + update `runs.status` in same tx
- [x] 3.2 Update `tests/m5_queue_projection.rs::outbox_dispatch_lifecycle_updates_projection_and_journal`
- [x] 3.3 Add new test `dispatch_success_completes_run` (in tests/m5_dispatch_outcome.rs)
- [x] 3.4 Run `validation_layout.py`

Stage 4 — Definite failure fails Run
- [x] 4.1 Modify `fail_outbox_dispatch` to write `RunFailed` + update `runs.status` in same tx
- [x] 4.2 Add new test `dispatch_definite_failure_fails_run`

Stage 5 — Unknown path documentation
- [x] 5.1 Confirm `unknown_outbox_dispatch` does NOT write RunCompleted (already true)
- [x] 5.2 Add test `dispatch_unknown_does_not_complete_run` asserting no RunCompleted

Stage 6 — Recovery rewrite
- [x] 6.1 Rewrite `recover_unknown_invocations` to write `OutboxDispatchUnknown` (not ReceiptReceived), no fail_run, no RunCompleted
- [x] 6.2 Add `stale_dispatching_with_terminal_journal` query and call it in recovery
- [x] 6.3 Update `tests/m0_kernel.rs::journal_recovery_marks_unknown_invocations` to assert `OutboxDispatchUnknown`
- [x] 6.4 Update `tests/m5_outbox_recovery.rs::unknown_recovery_updates_outbox_projection_from_dispatching`
- [x] 6.5 Rename `unknown_recovery_still_writes_receipt_for_journal_only_dispatch` to `...writes_outbox_dispatch_unknown...`
- [x] 6.6 Add new test `stale_dispatching_with_existing_terminal_event_only_fixes_projection`
- [x] 6.7 Add new test `stale_dispatching_unknown_never_returns_to_pending`
- [x] 6.8 Add new test `existing_outbox_dispatch_unknown_stops_scan`

Stage 7 — Dispatcher safety hints
- [x] 7.1 Add `outbox_status_count(status)` helper to `src/journal/queue_health.rs`
- [x] 7.2 Update `health_snapshot` signature to accept `outbox_dispatcher_enabled: bool`
- [x] 7.3 Add `outbox_dispatcher_enabled / outbox_pending_count / outbox_unknown_count / outbox_dispatching_count` to health JSON
- [x] 7.4 Update `handle_connection` `/health` branch to pass `config.outbox_dispatcher_enabled`
- [x] 7.5 Update `serve()` to print startup log before dispatcher loop
- [x] 7.6 Update test callers of `health_snapshot` (m0_kernel, m5_queue_projection, ingress_recovery)
- [x] 7.7 Add `health_fields_expose_dispatcher_state` test
- [x] 7.8 Add `disabled_dispatcher_loop_returns_without_draining_outbox` test

Stage 8 — Warning cleanup
- [x] 8.1 Remove unused `use rusqlite::params;` from `src/server/delivery.rs` test module

Stage 9 — Doc update
- [x] 9.1 Update `docs/architecture/outbox.md`: dispatch outcome -> Run status map, `OutboxDispatchUnknown` terminal semantics, dispatcher wired-in state
- [x] 9.2 `design-doc.html` left as-is (pre-existing stale doc, out of scope)

Stage 10 — Full verification
- [x] 10.1 Run `validation_layout.py`
- [x] 10.2 Run `cargo build`
- [x] 10.3 Run `cargo test`
- [x] 10.4 Run `pnpm check`
- [x] 10.5 Service restart skipped (no authorization to restart kernel during this run)

Stage 11 — Report
- [x] 11.1 Summarize change set, test result, health fields, TS connector untouched, no duplicate reply risk
- [ ] 11.2 Send completion notification via MCP `send_notification`
