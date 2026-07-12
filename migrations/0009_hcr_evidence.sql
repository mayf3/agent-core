-- HCR Durable Gate Evidence and Atomic Settlement (R3A-R2).
-- Evidence has NO cached result fields; settlement reads from journal events.

-- Gate Attempt: service-side canonical expectation for one gate execution.
CREATE TABLE IF NOT EXISTS hcr_gate_attempts (
    gate_attempt_id      TEXT NOT NULL PRIMARY KEY,
    hcr_id               TEXT NOT NULL REFERENCES harness_change_requests(request_id),
    claim_id             TEXT NOT NULL REFERENCES hcr_claims(claim_id),
    run_id               TEXT NOT NULL REFERENCES runs(id),
    harness_id           TEXT NOT NULL CHECK(length(harness_id) > 0),
    workspace_id         TEXT NOT NULL CHECK(length(workspace_id) > 0),
    gate_kind            TEXT NOT NULL CHECK (gate_kind IN ('scaffold','build','trusted_test','trusted_smoke','artifact')),
    expected_operation   TEXT NOT NULL CHECK(length(expected_operation) > 0),
    expected_profile     TEXT NOT NULL CHECK(length(expected_profile) > 0),
    invocation_intent_id TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    UNIQUE(hcr_id, claim_id, run_id, gate_kind),
    UNIQUE(invocation_intent_id)
);

-- Evidence: link between attempt and receipt event. NO cached result fields.
CREATE TABLE IF NOT EXISTS hcr_gate_evidence (
    evidence_id            TEXT NOT NULL PRIMARY KEY,
    gate_attempt_id        TEXT NOT NULL REFERENCES hcr_gate_attempts(gate_attempt_id),
    receipt_event_id       TEXT NOT NULL,
    receipt_payload_digest TEXT NOT NULL,
    created_at             TEXT NOT NULL,
    UNIQUE(gate_attempt_id),
    UNIQUE(receipt_event_id)
);

-- Settlement: single terminal result per HCR.
CREATE TABLE IF NOT EXISTS hcr_settlements (
    settlement_id           TEXT NOT NULL PRIMARY KEY,
    hcr_id                  TEXT NOT NULL REFERENCES harness_change_requests(request_id),
    claim_id                TEXT NOT NULL REFERENCES hcr_claims(claim_id),
    run_id                  TEXT NOT NULL REFERENCES runs(id),
    result                  TEXT NOT NULL CHECK (result IN ('succeeded','candidate_failed')),
    error_code              TEXT,
    evidence_set_digest     TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    UNIQUE(hcr_id)
);

CREATE INDEX IF NOT EXISTS idx_hcr_attempts_lookup
    ON hcr_gate_attempts(hcr_id, claim_id, run_id);

CREATE INDEX IF NOT EXISTS idx_hcr_evidence_attempt
    ON hcr_gate_evidence(gate_attempt_id);

CREATE INDEX IF NOT EXISTS idx_hcr_settlements_hcr
    ON hcr_settlements(hcr_id);
