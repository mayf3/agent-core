-- Capability Proposal HCR trusted-link table.
--
-- Binds a CapabilityChangeProposal to its originating HCR settlement,
-- providing a cryptographic chain of trust from the user's intent through
-- Harness execution, five acceptance gates, settlement, and final proposal.
--
-- Every key field is NOT NULL. Duplicate (hcr_id, candidate_digest, operation)
-- is prevented at the DB level.

CREATE TABLE IF NOT EXISTS capability_proposal_hcr_links (
    proposal_id             TEXT NOT NULL PRIMARY KEY CHECK(length(proposal_id) > 0),
    hcr_id                  TEXT NOT NULL CHECK(length(hcr_id) > 0),
    claim_id                TEXT NOT NULL CHECK(length(claim_id) > 0),
    run_id                  TEXT NOT NULL CHECK(length(run_id) > 0),
    operation               TEXT NOT NULL CHECK(length(operation) > 0),
    candidate_id            TEXT NOT NULL CHECK(length(candidate_id) > 0),
    candidate_digest        TEXT NOT NULL CHECK(length(candidate_digest) = 71),
    artifact_ref            TEXT NOT NULL CHECK(length(artifact_ref) = 71),
    artifact_digest         TEXT NOT NULL CHECK(length(artifact_digest) = 71),
    evidence_digest         TEXT NOT NULL CHECK(length(evidence_digest) = 71),
    source_registry_snapshot_id TEXT NOT NULL CHECK(length(source_registry_snapshot_id) > 0),
    settlement_id           TEXT NOT NULL CHECK(length(settlement_id) > 0),
    created_at              TEXT NOT NULL,
    UNIQUE(hcr_id, candidate_digest, operation)
) STRICT;

-- Existing v10 receipt rows predate candidate_id.  New PR3A receipts always
-- populate it, and trusted Proposal creation rejects the empty legacy default.
ALTER TABLE hcr_receipt_identities
    ADD COLUMN candidate_id TEXT NOT NULL DEFAULT '';
ALTER TABLE hcr_receipt_identities
    ADD COLUMN invocation_id TEXT NOT NULL DEFAULT '';

-- Durable ownership for the single controlled Harness submit.  This prevents
-- concurrent delivery/recovery of one Feishu message from invoking the
-- Harness more than once while still creating no HCR before submit succeeds.
CREATE TABLE IF NOT EXISTS coding_task_submissions (
    source_message_id TEXT NOT NULL PRIMARY KEY CHECK(length(source_message_id) > 0),
    request_digest    TEXT NOT NULL CHECK(length(request_digest) = 71),
    invocation_id     TEXT NOT NULL UNIQUE CHECK(length(invocation_id) > 0),
    origin_run_id     TEXT NOT NULL CHECK(length(origin_run_id) > 0),
    origin_session_id TEXT NOT NULL CHECK(length(origin_session_id) > 0),
    status            TEXT NOT NULL CHECK(status IN ('running','succeeded','failed')),
    result_json       TEXT,
    error_code        TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
) STRICT;
