-- Kernel-owned Approval identity for HCR-backed capability proposals.
--
-- The immutable fields bind a human decision to the exact trusted Proposal
-- chain.  Decision columns are intentionally nullable until the Approval is
-- consumed; their all-or-none constraint provides durable replay identity.

CREATE TABLE IF NOT EXISTS capability_change_approvals (
    approval_id                TEXT NOT NULL PRIMARY KEY
                                CHECK(length(approval_id) > 0),
    proposal_id                TEXT NOT NULL UNIQUE
                                REFERENCES capability_change_proposals(proposal_id),
    owner_principal_id         TEXT NOT NULL CHECK(length(owner_principal_id) > 0),
    source_registry_snapshot_id TEXT NOT NULL
                                CHECK(length(source_registry_snapshot_id) > 0),
    candidate_digest           TEXT NOT NULL
                                CHECK(length(candidate_digest) = 71
                                  AND substr(candidate_digest, 1, 7) = 'sha256:'
                                  AND substr(candidate_digest, 8) NOT GLOB '*[^0-9a-f]*'),
    artifact_digest            TEXT NOT NULL
                                CHECK(length(artifact_digest) = 71
                                  AND substr(artifact_digest, 1, 7) = 'sha256:'
                                  AND substr(artifact_digest, 8) NOT GLOB '*[^0-9a-f]*'),
    manifest_digest            TEXT NOT NULL
                                CHECK(length(manifest_digest) = 71
                                  AND substr(manifest_digest, 1, 7) = 'sha256:'
                                  AND substr(manifest_digest, 8) NOT GLOB '*[^0-9a-f]*'),
    decision_nonce             TEXT NOT NULL UNIQUE CHECK(length(decision_nonce) >= 32),
    status                     TEXT NOT NULL DEFAULT 'Pending'
                                CHECK(status IN
                                  ('Pending','Approved','Rejected','ActivationFailed','Expired')),
    decision_id                TEXT UNIQUE,
    decision_payload_digest    TEXT
                                CHECK(decision_payload_digest IS NULL
                                  OR (length(decision_payload_digest) = 71
                                    AND substr(decision_payload_digest, 1, 7) = 'sha256:'
                                    AND substr(decision_payload_digest, 8) NOT GLOB '*[^0-9a-f]*')),
    decision_result_json       TEXT,
    decided_at                 TEXT,
    decided_by                 TEXT,
    activated_snapshot_id      TEXT,
    host_deployment_id         TEXT,
    activation_error           TEXT,
    created_at                 TEXT NOT NULL,
    expires_at                 TEXT NOT NULL CHECK(expires_at > created_at),
    FOREIGN KEY(proposal_id) REFERENCES capability_proposal_hcr_links(proposal_id),
    CHECK (
        (status = 'Pending' AND decision_id IS NULL
          AND decision_payload_digest IS NULL AND decision_result_json IS NULL
          AND decided_at IS NULL AND decided_by IS NULL
          AND activated_snapshot_id IS NULL AND host_deployment_id IS NULL
          AND activation_error IS NULL)
        OR
        (status = 'Approved' AND decision_id IS NOT NULL AND length(decision_id) > 0
          AND decision_payload_digest IS NOT NULL AND decision_result_json IS NOT NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL AND length(decided_by) > 0
          AND activated_snapshot_id IS NOT NULL AND length(activated_snapshot_id) > 0
          AND host_deployment_id IS NOT NULL AND length(host_deployment_id) > 0
          AND activation_error IS NULL)
        OR
        (status = 'Rejected' AND decision_id IS NOT NULL AND length(decision_id) > 0
          AND decision_payload_digest IS NOT NULL AND decision_result_json IS NOT NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL AND length(decided_by) > 0
          AND activated_snapshot_id IS NULL AND host_deployment_id IS NULL
          AND activation_error IS NULL)
        OR
        (status = 'ActivationFailed' AND decision_id IS NOT NULL AND length(decision_id) > 0
          AND decision_payload_digest IS NOT NULL AND decision_result_json IS NOT NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL AND length(decided_by) > 0
          AND activated_snapshot_id IS NULL
          AND activation_error IS NOT NULL AND length(activation_error) > 0)
        OR
        (status = 'Expired' AND decision_id IS NULL
          AND decision_payload_digest IS NULL AND decision_result_json IS NULL
          AND decided_at IS NOT NULL AND decided_by IS NOT NULL AND length(decided_by) > 0
          AND activated_snapshot_id IS NULL AND host_deployment_id IS NULL
          AND activation_error IS NULL)
    )
) STRICT;

CREATE INDEX IF NOT EXISTS idx_capability_approvals_owner_status
    ON capability_change_approvals(owner_principal_id, status);

-- A v11 database may already contain trusted pending Proposals.  Give every
-- such Proposal a Kernel-owned Approval without changing or deleting any v11
-- row.  SQLite randomblob supplies independent 256-bit nonces.
INSERT OR IGNORE INTO capability_change_approvals (
    approval_id, proposal_id, owner_principal_id, source_registry_snapshot_id,
    candidate_digest, artifact_digest, manifest_digest, decision_nonce,
    status, created_at, expires_at
)
SELECT
    'approval_' || lower(hex(randomblob(16))),
    p.proposal_id,
    p.submitter_principal_id,
    l.source_registry_snapshot_id,
    l.candidate_digest,
    l.artifact_digest,
    p.manifest_digest,
    lower(hex(randomblob(32))),
    'Pending',
    p.created_at,
    p.expires_at
FROM capability_change_proposals p
JOIN capability_proposal_hcr_links l ON l.proposal_id = p.proposal_id
WHERE p.status = 'PendingApproval';

-- Approval binding is immutable even after a decision.  Only status and the
-- replay-result columns may transition during the later Decision transaction.
CREATE TRIGGER IF NOT EXISTS capability_approval_binding_immutable
BEFORE UPDATE OF
    approval_id, proposal_id, owner_principal_id, source_registry_snapshot_id,
    candidate_digest, artifact_digest, manifest_digest, decision_nonce,
    created_at, expires_at
ON capability_change_approvals
BEGIN
    SELECT RAISE(ABORT, 'CAPABILITY_APPROVAL_BINDING_IMMUTABLE');
END;
