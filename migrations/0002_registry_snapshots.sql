-- 0002_registry_snapshots.sql
-- Additive migration: immutable registry snapshots for run-scoped operation definitions.
-- Snapshots are append-only; operation rows are written once and never updated.

CREATE TABLE IF NOT EXISTS registry_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    operation_count INTEGER NOT NULL,
    canonical_digest TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS registry_snapshot_operations (
    snapshot_id TEXT NOT NULL,
    operation_name TEXT NOT NULL,
    risk TEXT NOT NULL,
    description TEXT NOT NULL,
    parameters_json TEXT NOT NULL,
    idempotent INTEGER NOT NULL,
    binding_kind TEXT NOT NULL,
    binding_key TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, operation_name),
    FOREIGN KEY (snapshot_id)
      REFERENCES registry_snapshots(snapshot_id)
      ON DELETE RESTRICT
);

ALTER TABLE runs ADD COLUMN registry_snapshot_id TEXT;
