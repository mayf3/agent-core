-- 0003_external_harness_hotload.sql
-- Additive migration: external harness manifests and active registry state.
-- Manifests are immutable once registered; registry_state tracks the active
-- snapshot across restarts.

CREATE TABLE IF NOT EXISTS harness_manifests (
    manifest_id TEXT PRIMARY KEY,
    harness_id TEXT NOT NULL,
    artifact_digest TEXT NOT NULL,
    protocol_version TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    operation_name TEXT NOT NULL,
    description TEXT NOT NULL,
    input_schema_json TEXT NOT NULL,
    output_schema_json TEXT NOT NULL,
    idempotent INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    canonical_digest TEXT NOT NULL UNIQUE,
    UNIQUE (manifest_id),
    UNIQUE (operation_name)
);

CREATE TABLE IF NOT EXISTS registry_state (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    active_snapshot_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (active_snapshot_id)
        REFERENCES registry_snapshots(snapshot_id)
        ON DELETE RESTRICT
);
