-- 0019_registry_hook_bindings.sql
-- Additive migration: generic hook bindings frozen per registry snapshot.
-- One binding per (snapshot_id, contract); the Kernel resolves the binding
-- for a lifecycle point by contract and fails closed when the contract is
-- missing or duplicated.
--
-- New hook contracts (e.g. context.prepare.v0) are plain data values in this
-- table — no schema change is needed to add one.
--
-- Historical snapshots are NOT backfilled or modified in place. The boot
-- path creates a NEW snapshot with the bootstrap binding set for the active
-- state when the active snapshot predates this feature.

CREATE TABLE IF NOT EXISTS registry_snapshot_hook_bindings (
    snapshot_id TEXT NOT NULL,
    contract TEXT NOT NULL,
    hook_id TEXT NOT NULL,
    hook_version TEXT NOT NULL,
    binding_kind TEXT NOT NULL,
    binding_key TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, contract),
    FOREIGN KEY (snapshot_id)
      REFERENCES registry_snapshots(snapshot_id)
      ON DELETE RESTRICT
);
