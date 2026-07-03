-- 0004_capability_change_proposals.sql
-- Additive migration: capability change proposals for trusted approval pipeline.

CREATE TABLE IF NOT EXISTS capability_change_proposals (
    proposal_id TEXT PRIMARY KEY,

    submitter_principal_id TEXT NOT NULL,
    target_agent_id TEXT NOT NULL,
    origin_session_id TEXT NOT NULL,
    origin_run_id TEXT NOT NULL,

    artifact_ref TEXT NOT NULL,
    artifact_digest TEXT NOT NULL,
    manifest_ref TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    evidence_ref TEXT NOT NULL,
    evidence_digest TEXT NOT NULL,

    requested_operations_json TEXT NOT NULL,
    risk_summary TEXT NOT NULL DEFAULT '',
    expected_active_snapshot_id TEXT NOT NULL,

    status TEXT NOT NULL DEFAULT 'PendingApproval',
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,

    decided_at TEXT,
    decided_by TEXT,
    decision_reason TEXT,

    activated_snapshot_id TEXT,
    activation_error TEXT
);

CREATE INDEX IF NOT EXISTS idx_proposals_status ON capability_change_proposals(status);
CREATE INDEX IF NOT EXISTS idx_proposals_session ON capability_change_proposals(origin_session_id);
