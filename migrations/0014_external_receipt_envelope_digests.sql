-- External Receipt Envelope digest columns (H1/H2).
--
-- Adds receipt_digest and opaque_payload_digest columns to the
-- hcr_receipt_identities table. These capture the envelope-level
-- digests from the ExternalReceiptEnvelope protocol.
--
-- Backward compatible: existing rows get empty defaults.
-- All new rows will have non-empty values.

ALTER TABLE hcr_receipt_identities
    ADD COLUMN receipt_digest TEXT NOT NULL DEFAULT '';

ALTER TABLE hcr_receipt_identities
    ADD COLUMN opaque_payload_digest TEXT NOT NULL DEFAULT '';
