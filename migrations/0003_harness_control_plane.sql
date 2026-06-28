-- Migration 0003: Harness Control Plane
-- Adds tables for external harness bundle registration, runtime
-- registration, and explicit channel-level operation grants.

CREATE TABLE IF NOT EXISTS harness_bundles (
    bundle_hash       TEXT PRIMARY KEY,
    manifest_version  TEXT NOT NULL,
    protocol_version  TEXT NOT NULL,
    bundle_id         TEXT NOT NULL,
    bundle_version    TEXT NOT NULL,
    manifest_json     TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    UNIQUE(bundle_id, bundle_version)
);

CREATE TABLE IF NOT EXISTS harness_runtime_registrations (
    registration_id  TEXT PRIMARY KEY,
    bundle_hash      TEXT NOT NULL REFERENCES harness_bundles(bundle_hash),
    endpoint         TEXT NOT NULL,
    transport        TEXT NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    registered_at    TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    UNIQUE(bundle_hash)
);

CREATE TABLE IF NOT EXISTS channel_operation_grants (
    channel         TEXT NOT NULL,
    operation_name  TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    PRIMARY KEY(channel, operation_name)
);
