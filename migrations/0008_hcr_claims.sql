-- HCR Claims and Run Bindings v0 (R2).
--
-- Claim is the first stateful action for an HCR. A successful claim atomically
-- transitions the HCR from 'pending' to 'running' and creates a claim record.
--
-- At most one active claim per HCR (UNIQUE on hcr_id).
-- At most one active Run per claim (UNIQUE on (hcr_id, claim_id) and UNIQUE on run_id).
--
-- Also adds a `mode` column to the `runs` table for trusted RunMode storage.
--
-- See docs/architecture/hcr-claim-run-binding-v0.md.

-- Add mode column to existing runs table (default 'default' for all existing rows).
ALTER TABLE runs ADD COLUMN mode TEXT NOT NULL DEFAULT 'default';

CREATE TABLE IF NOT EXISTS hcr_claims (
    claim_id            TEXT NOT NULL PRIMARY KEY,
    hcr_id              TEXT NOT NULL,
    harness_id          TEXT NOT NULL,
    worker_instance_id  TEXT NOT NULL,
    claimed_at          TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'active'
                        CHECK (status IN ('active', 'released')),
    -- One active claim per HCR (unique constraint prevents double-claim).
    UNIQUE(hcr_id)
);

CREATE TABLE IF NOT EXISTS hcr_run_bindings (
    binding_id  TEXT NOT NULL PRIMARY KEY,
    hcr_id      TEXT NOT NULL,
    claim_id    TEXT NOT NULL,
    run_id      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    -- One run per claim.
    UNIQUE(hcr_id, claim_id),
    -- One claim per run.
    UNIQUE(run_id)
);

-- Index for looking up claim by HCR id.
CREATE INDEX IF NOT EXISTS idx_hcr_claims_hcr_id
    ON hcr_claims(hcr_id);

-- Index for looking up binding by run_id.
CREATE INDEX IF NOT EXISTS idx_hcr_run_bindings_run_id
    ON hcr_run_bindings(run_id);
