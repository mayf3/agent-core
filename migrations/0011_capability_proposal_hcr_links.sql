-- Capability Proposal HCR trusted-link table.
--
-- Binds a CapabilityChangeProposal to its originating HCR settlement,
-- providing a cryptographic chain of trust from the user's intent through
-- Harness execution, five acceptance gates, settlement, and final proposal.
--
-- Every key field is NOT NULL. Duplicate (hcr_id, candidate_digest, operation)
-- is prevented at the DB level.

CREATE TABLE IF NOT EXISTS capability_proposal_hcr_links (
    proposal_id             TEXT NOT NULL PRIMARY KEY,
    hcr_id                  TEXT NOT NULL,
    claim_id                TEXT NOT NULL,
    run_id                  TEXT NOT NULL,
    operation               TEXT NOT NULL,
    candidate_id            TEXT NOT NULL,
    candidate_digest        TEXT NOT NULL,
    artifact_ref            TEXT NOT NULL,
    artifact_digest         TEXT NOT NULL,
    evidence_digest         TEXT NOT NULL,
    source_registry_snapshot_id TEXT NOT NULL,
    settlement_id           TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    UNIQUE(hcr_id, candidate_digest, operation)
) STRICT;
