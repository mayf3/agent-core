-- HCR Durable Gate Evidence and Atomic Settlement (R3A-R1).
--
-- This migration adds:
-- 1. hcr_gate_attempts — canonical service-side gate definition
-- 2. hcr_gate_evidence — persistent gate execution result bound to receipt
-- 3. hcr_settlements — single terminal settlement record per HCR
-- 4. PRAGMA foreign_keys — enable referential integrity
--
-- All tables use FOREIGN KEY references to existing tables where possible.
--
-- InvocationIntent and Receipt live only in journal_events; their FK is
-- enforced by application logic (event kind + correlation_id), not by DB.

PRAGMA foreign_keys = ON;

-- Gate Attempt: service-side canonical expectation for one gate execution.
CREATE TABLE IF NOT EXISTS hcr_gate_attempts (
    gate_attempt_id      TEXT NOT NULL PRIMARY KEY,
    hcr_id               TEXT NOT NULL REFERENCES harness_change_requests(request_id),
    claim_id             TEXT NOT NULL REFERENCES hcr_claims(claim_id),
    run_id               TEXT NOT NULL REFERENCES runs(id),
    harness_id           TEXT NOT NULL,
    workspace_id         TEXT NOT NULL,
    gate_kind            TEXT NOT NULL CHECK (gate_kind IN ('scaffold', 'build', 'trusted_test', 'trusted_smoke', 'artifact')),
    expected_operation   TEXT NOT NULL CHECK(length(expected_operation) > 0),
    expected_profile     TEXT NOT NULL CHECK(length(expected_profile) > 0),
    invocation_intent_id TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    -- One attempt per gate kind per HCR/claim/run.
    UNIQUE(hcr_id, claim_id, run_id, gate_kind),
    -- One invocation intent per attempt.
    UNIQUE(invocation_intent_id)
);

-- Evidence: canonical result of a gate execution, bound to the receipt.
-- Structured fields are cached from the receipt for fast settlement queries,
-- but settlement MUST re-verify against the original journal events.
CREATE TABLE IF NOT EXISTS hcr_gate_evidence (
    evidence_id          TEXT NOT NULL PRIMARY KEY,
    gate_attempt_id      TEXT NOT NULL REFERENCES hcr_gate_attempts(gate_attempt_id),
    receipt_event_id     TEXT NOT NULL,
    structured_status    TEXT NOT NULL,
    exit_code            INTEGER NOT NULL,
    timed_out            INTEGER NOT NULL DEFAULT 0,
    stdout_truncated     INTEGER NOT NULL DEFAULT 0,
    stderr_truncated     INTEGER NOT NULL DEFAULT 0,
    child_cleanup        INTEGER,
    error_code           TEXT,
    receipt_payload_digest TEXT NOT NULL DEFAULT '',
    created_at           TEXT NOT NULL,
    -- One evidence per attempt.
    UNIQUE(gate_attempt_id),
    -- One receipt per evidence.
    UNIQUE(receipt_event_id)
);

-- Settlement: single terminal result per HCR.
CREATE TABLE IF NOT EXISTS hcr_settlements (
    settlement_id           TEXT NOT NULL PRIMARY KEY,
    hcr_id                  TEXT NOT NULL REFERENCES harness_change_requests(request_id),
    claim_id                TEXT NOT NULL,
    run_id                  TEXT NOT NULL,
    result                  TEXT NOT NULL CHECK (result IN ('succeeded', 'candidate_failed')),
    error_code              TEXT,
    evidence_set_digest     TEXT NOT NULL,
    terminal_event_sequence INTEGER,
    created_at              TEXT NOT NULL,
    -- One settlement per HCR.
    UNIQUE(hcr_id)
);

CREATE INDEX IF NOT EXISTS idx_hcr_attempts_lookup
    ON hcr_gate_attempts(hcr_id, claim_id, run_id);

CREATE INDEX IF NOT EXISTS idx_hcr_evidence_attempt
    ON hcr_gate_evidence(gate_attempt_id);

CREATE INDEX IF NOT EXISTS idx_hcr_settlements_hcr
    ON hcr_settlements(hcr_id);
