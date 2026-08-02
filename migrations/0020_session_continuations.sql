-- 0020: session continuation ledger
--
-- Generic governance ledger for same-session continuation requests made by
-- an external Agent Loop Harness (POST /v1/session-continuation).
--
-- This is NOT a task/progress/checkpoint table. It only guarantees:
--   1. UNIQUE(trigger_run_id) — the SAME trigger Run can be continued at most
--      ONCE, regardless of idempotency_key, concurrency, Harness state loss,
--      or Harness restart. A duplicate request returns the already-created
--      next_run_id instead of enqueueing a second worker job;
--   2. auditability — the Kernel can prove which external continuation
--      requests were accepted, with which key, and which next Run they
--      produced (next_run_id is backfilled when the worker schedules the Run).
--
-- All product semantics (whether to continue, how far, what to do next) are
-- decided OUTSIDE the Kernel by the Agent Loop Harness. The Kernel only
-- records the generic fact that an authorized continuation was accepted.

CREATE TABLE IF NOT EXISTS session_continuations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Deterministic key supplied by the Harness: "continuation:<trigger_run_id>".
    idempotency_key TEXT    NOT NULL UNIQUE,
    session_id      TEXT    NOT NULL,
    -- A trigger Run may be continued at most once.
    trigger_run_id  TEXT    NOT NULL UNIQUE,
    event_id        TEXT    NOT NULL,
    -- Backfilled when the worker schedules the continuation Run.
    next_run_id     TEXT,
    created_at      TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_continuations_session
    ON session_continuations (session_id);
