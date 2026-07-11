-- HarnessChangeRequest v0: durable pending request for harness creation.
--
-- Used by PR4A1 to receive, authorize, validate, deduplicate, and persist
-- HarnessChangeRequest records WITHOUT creating a Run or starting execution.
-- PR4A2 will consume pending requests, create Runs, and drive the scaffold.
--
-- The UNIQUE(source, source_message_id) constraint provides idempotent
-- duplicate delivery: the same Feishu message_id always maps to the same
-- request_id.
--
-- Status values (v0):
--   pending   – request received and persisted, awaiting driver execution (PR4A2)
--   running   – driver has picked up this request and created a Run (PR4A2)
--   succeeded – request completed successfully (PR4A2)
--   failed    – request failed (PR4A2)
--   cancelled – request was cancelled before execution (PR4A2)

CREATE TABLE IF NOT EXISTS harness_change_requests (
    request_id          TEXT NOT NULL PRIMARY KEY,
    source              TEXT NOT NULL,
    source_message_id   TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    principal_id        TEXT NOT NULL,
    channel             TEXT NOT NULL,
    chat_type           TEXT NOT NULL,
    harness_id          TEXT NOT NULL,
    requirement         TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled')),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    run_id              TEXT,
    error_code          TEXT
);

-- Unique constraint for idempotent duplicate delivery.
CREATE UNIQUE INDEX IF NOT EXISTS idx_hcr_source_dedup
    ON harness_change_requests(source, source_message_id);

-- Index by status for PR4A2 driver to find pending requests.
CREATE INDEX IF NOT EXISTS idx_hcr_status
    ON harness_change_requests(status);
