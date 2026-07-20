-- Delivery manifest columns for external development boundary (V1).
--
-- Adds delivery_manifest_ref and delivery_manifest_digest columns to the
-- hcr_receipt_identities table. These capture the content-addressed ref
-- and ContentStore digest of the final delivery manifest constructed by
-- the Coding Harness (instead of the Kernel).
--
-- Backward compatible: existing rows get empty defaults.
-- After the Kernel cleanup, all new HookConsumerService receipts will
-- have non-empty values.

ALTER TABLE hcr_receipt_identities
    ADD COLUMN delivery_manifest_ref TEXT NOT NULL DEFAULT '';

ALTER TABLE hcr_receipt_identities
    ADD COLUMN delivery_manifest_digest TEXT NOT NULL DEFAULT '';
