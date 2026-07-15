-- Immutable managed-component snapshots and deployment receipts.

CREATE TABLE IF NOT EXISTS component_registry_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    component_count INTEGER NOT NULL,
    canonical_digest TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE IF NOT EXISTS component_registry_entries (
    snapshot_id TEXT NOT NULL,
    component_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    manifest_id TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    artifact_digest TEXT NOT NULL,
    version TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    deployment_receipt_id TEXT NOT NULL,
    status TEXT NOT NULL,
    required_contracts_json TEXT NOT NULL,
    requested_permissions_json TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, component_id),
    FOREIGN KEY (snapshot_id) REFERENCES component_registry_snapshots(snapshot_id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE IF NOT EXISTS component_registry_state (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    active_snapshot_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (active_snapshot_id) REFERENCES component_registry_snapshots(snapshot_id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE IF NOT EXISTS component_deployment_intents (
    intent_id TEXT PRIMARY KEY,
    invocation_id TEXT NOT NULL UNIQUE,
    proposal_id TEXT NOT NULL,
    decision_id TEXT NOT NULL UNIQUE,
    component_id TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    artifact_digest TEXT NOT NULL,
    expected_version TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS component_deployment_receipts (
    receipt_id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL UNIQUE,
    invocation_id TEXT NOT NULL UNIQUE,
    proposal_id TEXT NOT NULL,
    decision_id TEXT NOT NULL,
    component_id TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    artifact_digest TEXT NOT NULL,
    version TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    health_status TEXT NOT NULL,
    log_ref TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS component_control_intents (
    decision_id TEXT PRIMARY KEY,
    component_id TEXT NOT NULL,
    action TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    expected_snapshot_id TEXT NOT NULL,
    expected_deployment_id TEXT NOT NULL,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS component_control_receipts (
    receipt_id TEXT PRIMARY KEY,
    decision_id TEXT NOT NULL UNIQUE,
    component_id TEXT NOT NULL,
    action TEXT NOT NULL,
    target_snapshot_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (decision_id) REFERENCES component_control_intents(decision_id) ON DELETE RESTRICT,
    FOREIGN KEY (target_snapshot_id) REFERENCES component_registry_snapshots(snapshot_id) ON DELETE RESTRICT
) STRICT;
