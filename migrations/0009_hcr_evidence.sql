-- HCR Durable Gate Evidence and Atomic Settlement (R3A).
--
-- This migration adds:
-- 1. hcr_gate_evidence — persistent record of each gate execution,
--    bound to its invocation intent and receipt.
-- 2. hcr_settlements — single terminal settlement record per HCR,
--    created atomically with the HCR status update and journal event.

-- Evidence table: each row is one canonical gate execution result.
CREATE TABLE IF NOT EXISTS hcr_gate_evidence (
    evidence_id          TEXT NOT NULL PRIMARY KEY,
    hcr_id               TEXT NOT NULL,
    claim_id             TEXT NOT NULL,
    run_id               TEXT NOT NULL,
    harness_id           TEXT NOT NULL,
    workspace_id         TEXT NOT NULL,
    gate_kind            TEXT NOT NULL CHECK (gate_kind IN ('scaffold', 'build', 'trusted_test', 'trusted_smoke', 'artifact')),
    invocation_intent_id TEXT NOT NULL,
    receipt_id           TEXT NOT NULL,
    operation            TEXT NOT NULL DEFAULT '',
    execution_profile    TEXT NOT NULL DEFAULT '',
    structured_status    TEXT NOT NULL,
    exit_code            INTEGER NOT NULL,
    timed_out            INTEGER NOT NULL DEFAULT 0,
    stdout_truncated     INTEGER NOT NULL DEFAULT 0,
    stderr_truncated     INTEGER NOT NULL DEFAULT 0,
    child_cleanup        INTEGER,
    error_code           TEXT,
    artifact_digest      TEXT,
    manifest_digest      TEXT,
    created_at           TEXT NOT NULL,
    -- One evidence per gate kind per HCR/claim/run.
    UNIQUE(hcr_id, claim_id, run_id, gate_kind),
    -- One receipt per evidence.
    UNIQUE(receipt_id),
    -- One invocation intent per evidence.
    UNIQUE(invocation_intent_id)
);

-- Settlement table: each HCR has at most one terminal settlement.
CREATE TABLE IF NOT EXISTS hcr_settlements (
    settlement_id   TEXT NOT NULL PRIMARY KEY,
    hcr_id          TEXT NOT NULL,
    claim_id        TEXT NOT NULL,
    run_id          TEXT NOT NULL,
    result          TEXT NOT NULL CHECK (result IN ('succeeded', 'candidate_failed')),
    error_code      TEXT,
    evidence_set_digest TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    -- One settlement per HCR.
    UNIQUE(hcr_id)
);

CREATE INDEX IF NOT EXISTS idx_hcr_evidence_lookup
    ON hcr_gate_evidence(hcr_id, claim_id, run_id);

CREATE INDEX IF NOT EXISTS idx_hcr_evidence_receipt
    ON hcr_gate_evidence(receipt_id);

CREATE INDEX IF NOT EXISTS idx_hcr_settlements_hcr
    ON hcr_settlements(hcr_id);
