-- HCR Receipt Identity uniqueness table (H3/H6).
--
-- Provides a database-level UNIQUE constraint on the receipt identity key
-- (hcr_id, claim_id, run_id, idempotency_key) with a canonical payload
-- digest for conflict detection.
--
-- The UNIQUE constraint prevents duplicate or conflicting receipts from
-- being appended atomically, even across concurrent connections.

CREATE TABLE IF NOT EXISTS hcr_receipt_identities (
    -- Unique identity key components (together form the UNIQUE constraint)
    hcr_id              TEXT NOT NULL,
    claim_id            TEXT NOT NULL,
    run_id              TEXT NOT NULL,
    idempotency_key     TEXT NOT NULL,

    -- Canonical payload digest for conflict comparison
    payload_digest      TEXT NOT NULL,

    -- Reference to the journal event
    receipt_event_id    TEXT NOT NULL,

    -- Stable fields from the acceptance response
    harness_execution_id TEXT NOT NULL,
    overall_outcome     TEXT NOT NULL,
    candidate_digest    TEXT NOT NULL,
    artifact_ref        TEXT,
    artifact_digest     TEXT,
    evidence_digest     TEXT NOT NULL,

    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    -- Unique constraint: only one receipt per identity key
    UNIQUE(hcr_id, claim_id, run_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_hcr_receipt_identity_key
    ON hcr_receipt_identities(hcr_id, claim_id, run_id, idempotency_key);
