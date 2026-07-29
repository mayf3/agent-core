-- Generic operator reconciliation for HCRs whose execution has already
-- produced a trustworthy terminal failure fact but did not settle.
--
-- SQLite cannot widen a CHECK constraint in place, so preserve every v15
-- settlement while rebuilding the table with one additional terminal result
-- and an optional pointer to the pre-existing failure event.

-- Historical databases can contain pre-trigger rows whose referenced HCR or
-- claim was later removed. Preserve those rows byte-for-byte; the new insert
-- triggers below continue to reject any new ghost settlement.
PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

ALTER TABLE hcr_settlements RENAME TO hcr_settlements_v15;

CREATE TABLE hcr_settlements (
    settlement_id              TEXT NOT NULL PRIMARY KEY,
    hcr_id                     TEXT NOT NULL REFERENCES harness_change_requests(request_id),
    claim_id                   TEXT NOT NULL REFERENCES hcr_claims(claim_id),
    run_id                     TEXT NOT NULL REFERENCES runs(id),
    result                     TEXT NOT NULL
                               CHECK (result IN (
                                   'succeeded',
                                   'candidate_failed',
                                   'infrastructure_failed'
                               )),
    error_code                 TEXT,
    evidence_set_digest        TEXT NOT NULL,
    failure_evidence_event_id  TEXT,
    created_at                 TEXT NOT NULL,
    UNIQUE(hcr_id)
);

INSERT INTO hcr_settlements (
    settlement_id, hcr_id, claim_id, run_id, result, error_code,
    evidence_set_digest, failure_evidence_event_id, created_at
)
SELECT
    settlement_id, hcr_id, claim_id, run_id, result, error_code,
    evidence_set_digest, NULL, created_at
FROM hcr_settlements_v15;

DROP TABLE hcr_settlements_v15;

CREATE INDEX idx_hcr_settlements_hcr
    ON hcr_settlements(hcr_id);

CREATE TRIGGER trg_settlement_hcr_exists
BEFORE INSERT ON hcr_settlements
WHEN NOT EXISTS (SELECT 1 FROM harness_change_requests WHERE request_id = NEW.hcr_id)
BEGIN SELECT RAISE(ABORT, 'GHOST_HCR_IN_SETTLEMENT'); END;

CREATE TRIGGER trg_settlement_claim_exists
BEFORE INSERT ON hcr_settlements
WHEN NOT EXISTS (SELECT 1 FROM hcr_claims WHERE claim_id = NEW.claim_id)
BEGIN SELECT RAISE(ABORT, 'GHOST_CLAIM_IN_SETTLEMENT'); END;

CREATE TRIGGER trg_settlement_run_exists
BEFORE INSERT ON hcr_settlements
WHEN NOT EXISTS (SELECT 1 FROM runs WHERE id = NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'GHOST_RUN_IN_SETTLEMENT'); END;

COMMIT;
PRAGMA foreign_keys = ON;
