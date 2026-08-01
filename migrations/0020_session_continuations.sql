-- 0020: session continuation ledger
--
-- Generic governance ledger for same-session continuation requests made by
-- an external Agent Loop Harness (POST /v1/session-continuation).
--
-- This is NOT a task/progress/checkpoint table. It only guarantees:
--   1. idempotency — a retried request with the same idempotency_key never
--      creates a second next Run (a duplicate POST returns the original
--      event_id instead of enqueueing a new worker job);
--   2. auditability — the Kernel can prove which external continuation
--      requests were accepted and which Run/event they produced.
--
-- All product semantics (whether to continue, how far, what to do next) are
-- decided OUTSIDE the Kernel by the Agent Loop Harness. The Kernel only
-- records the generic fact that an authorized continuation was accepted.

CREATE TABLE IF NOT EXISTS session_continuations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    idempotency_key TEXT    NOT NULL UNIQUE,
    session_id      TEXT    NOT NULL,
    trigger_run_id  TEXT    NOT NULL,
    source          TEXT    NOT NULL,
    event_id        TEXT    NOT NULL,
    created_at      TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_continuations_session
    ON session_continuations (session_id);
