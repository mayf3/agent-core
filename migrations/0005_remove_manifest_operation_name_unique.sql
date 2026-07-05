-- 0005_remove_manifest_operation_name_unique.sql
-- Migration: remove UNIQUE(operation_name) from harness_manifests to allow
-- multiple manifest versions for the same operation (schema-only upgrades).
-- SQLite does not support ALTER TABLE DROP CONSTRAINT, so we recreate the table.

CREATE TABLE IF NOT EXISTS harness_manifests_v5 (
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
    canonical_digest TEXT NOT NULL UNIQUE
);

INSERT INTO harness_manifests_v5
    (manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
     operation_name, description, input_schema_json, output_schema_json,
     idempotent, created_at, canonical_digest)
SELECT
    manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
    operation_name, description, input_schema_json, output_schema_json,
    idempotent, created_at, canonical_digest
FROM harness_manifests;

DROP TABLE harness_manifests;

ALTER TABLE harness_manifests_v5 RENAME TO harness_manifests;

CREATE INDEX IF NOT EXISTS idx_harness_manifests_operation_name
    ON harness_manifests(operation_name);
