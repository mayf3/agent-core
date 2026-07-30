-- Generic receipt-bound capability governance.
--
-- This migration is deliberately additive. Historical HCR rows, links and
-- approvals stay intact and readable. New development proposals use the
-- receipt link and governance approval tables below and never create HCR
-- workflow facts.

CREATE TABLE IF NOT EXISTS capability_proposal_receipt_links (
    proposal_id                TEXT NOT NULL PRIMARY KEY
                               REFERENCES capability_change_proposals(proposal_id),
    request_id                 TEXT NOT NULL CHECK(length(request_id) > 0),
    request_digest             TEXT NOT NULL CHECK(length(request_digest) = 71),
    acceptance_invocation_id   TEXT NOT NULL CHECK(length(acceptance_invocation_id) > 0),
    issuer_principal_id        TEXT NOT NULL CHECK(length(issuer_principal_id) > 0),
    operation                  TEXT NOT NULL CHECK(length(operation) > 0),
    candidate_id               TEXT NOT NULL CHECK(length(candidate_id) > 0),
    candidate_digest           TEXT NOT NULL CHECK(length(candidate_digest) = 71),
    artifact_ref               TEXT NOT NULL CHECK(length(artifact_ref) = 71),
    artifact_digest            TEXT NOT NULL CHECK(length(artifact_digest) = 71),
    manifest_ref               TEXT NOT NULL CHECK(length(manifest_ref) > 0),
    manifest_digest            TEXT NOT NULL CHECK(length(manifest_digest) = 71),
    evidence_digest            TEXT NOT NULL CHECK(length(evidence_digest) = 71),
    receipt_digest             TEXT NOT NULL UNIQUE CHECK(length(receipt_digest) = 71),
    acceptance_outcome         TEXT NOT NULL CHECK(acceptance_outcome IN ('passed','failed')),
    contract_catalog_version   TEXT NOT NULL CHECK(length(contract_catalog_version) > 0),
    profile_id                 TEXT NOT NULL CHECK(length(profile_id) > 0),
    profile_catalog_version    TEXT NOT NULL CHECK(length(profile_catalog_version) > 0),
    source_registry_snapshot_id TEXT NOT NULL
                               CHECK(length(source_registry_snapshot_id) > 0),
    origin_run_id              TEXT NOT NULL CHECK(length(origin_run_id) > 0),
    origin_session_id          TEXT NOT NULL CHECK(length(origin_session_id) > 0),
    created_at                 TEXT NOT NULL,
    UNIQUE(request_digest, operation)
) STRICT;

-- One active generic Approval authority. Historical rows are copied from the
-- old HCR-bound table without deleting or rewriting the old table.
CREATE TABLE IF NOT EXISTS capability_governance_approvals (
    approval_id                 TEXT NOT NULL PRIMARY KEY
                                CHECK(length(approval_id) > 0),
    proposal_id                 TEXT NOT NULL UNIQUE
                                REFERENCES capability_change_proposals(proposal_id),
    owner_principal_id          TEXT NOT NULL CHECK(length(owner_principal_id) > 0),
    source_registry_snapshot_id TEXT NOT NULL
                                CHECK(length(source_registry_snapshot_id) > 0),
    candidate_digest            TEXT NOT NULL CHECK(length(candidate_digest) = 71),
    artifact_digest             TEXT NOT NULL CHECK(length(artifact_digest) = 71),
    manifest_digest             TEXT NOT NULL CHECK(length(manifest_digest) = 71),
    decision_nonce              TEXT NOT NULL UNIQUE CHECK(length(decision_nonce) >= 32),
    status                      TEXT NOT NULL DEFAULT 'Pending'
                                CHECK(status IN
                                  ('Pending','Approved','Rejected','ActivationFailed','Expired')),
    decision_id                 TEXT UNIQUE,
    decision_payload_digest     TEXT,
    decision_result_json        TEXT,
    decided_at                  TEXT,
    decided_by                  TEXT,
    activated_snapshot_id       TEXT,
    host_deployment_id          TEXT,
    activation_error            TEXT,
    created_at                  TEXT NOT NULL,
    expires_at                  TEXT NOT NULL CHECK(expires_at > created_at),
    CHECK (
        (status = 'Pending' AND decision_id IS NULL
          AND decision_payload_digest IS NULL AND decision_result_json IS NULL
          AND decided_at IS NULL AND decided_by IS NULL
          AND activated_snapshot_id IS NULL AND host_deployment_id IS NULL
          AND activation_error IS NULL)
        OR
        (status = 'Approved' AND decision_id IS NOT NULL
          AND decision_payload_digest IS NOT NULL AND decision_result_json IS NOT NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL
          AND activated_snapshot_id IS NOT NULL AND host_deployment_id IS NOT NULL
          AND activation_error IS NULL)
        OR
        (status = 'Rejected' AND decision_id IS NOT NULL
          AND decision_payload_digest IS NOT NULL AND decision_result_json IS NOT NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL
          AND activated_snapshot_id IS NULL AND host_deployment_id IS NULL
          AND activation_error IS NULL)
        OR
        (status = 'ActivationFailed' AND decision_id IS NOT NULL
          AND decision_payload_digest IS NOT NULL AND decision_result_json IS NOT NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL
          AND activated_snapshot_id IS NULL AND activation_error IS NOT NULL)
        OR
        (status = 'Expired' AND decision_id IS NULL
          AND decision_payload_digest IS NULL AND decision_result_json IS NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL
          AND activated_snapshot_id IS NULL AND host_deployment_id IS NULL
          AND activation_error IS NULL)
    )
) STRICT;

INSERT OR IGNORE INTO capability_governance_approvals (
    approval_id,proposal_id,owner_principal_id,source_registry_snapshot_id,
    candidate_digest,artifact_digest,manifest_digest,decision_nonce,status,
    decision_id,decision_payload_digest,decision_result_json,decided_at,decided_by,
    activated_snapshot_id,host_deployment_id,activation_error,created_at,expires_at
)
SELECT approval_id,proposal_id,owner_principal_id,source_registry_snapshot_id,
       candidate_digest,artifact_digest,manifest_digest,decision_nonce,status,
       decision_id,decision_payload_digest,decision_result_json,decided_at,decided_by,
       activated_snapshot_id,host_deployment_id,activation_error,created_at,expires_at
FROM capability_change_approvals;

CREATE INDEX IF NOT EXISTS idx_capability_governance_approvals_owner_status
    ON capability_governance_approvals(owner_principal_id, status);

CREATE TRIGGER IF NOT EXISTS capability_proposal_receipt_link_immutable_update
BEFORE UPDATE ON capability_proposal_receipt_links
BEGIN
    SELECT RAISE(ABORT, 'CAPABILITY_PROPOSAL_RECEIPT_LINK_IMMUTABLE');
END;

CREATE TRIGGER IF NOT EXISTS capability_proposal_receipt_link_immutable_delete
BEFORE DELETE ON capability_proposal_receipt_links
BEGIN
    SELECT RAISE(ABORT, 'CAPABILITY_PROPOSAL_RECEIPT_LINK_IMMUTABLE');
END;

CREATE TRIGGER IF NOT EXISTS capability_governance_approval_binding_immutable
BEFORE UPDATE OF
    approval_id,proposal_id,owner_principal_id,source_registry_snapshot_id,
    candidate_digest,artifact_digest,manifest_digest,decision_nonce,created_at,expires_at
ON capability_governance_approvals
BEGIN
    SELECT RAISE(ABORT, 'CAPABILITY_GOVERNANCE_APPROVAL_BINDING_IMMUTABLE');
END;
