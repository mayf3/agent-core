-- External operation grants: explicit principal-level authorization for
-- non-coding external harness operations (e.g. external.calculator).
--
-- Separates grant authorization from capability activation: activating a
-- capability only registers the operation in the registry snapshot; a grant
-- is required for a specific principal to invoke it.
--
-- The partial unique index idx_ext_op_grants_active_unique prevents
-- duplicate active grants for the same logical tuple. The journal
-- create method uses INSERT OR IGNORE so repeated calls are safe, and on
-- duplicate returns the existing persistent grant_id (not a fake UUID).
--
-- Revocation: the grant row is retained (status='revoked', revoked_at set)
-- for audit trail — no row is ever deleted. The partial index does NOT
-- cover revoked rows, so any number of revoked history rows for the same
-- logical tuple can coexist for audit.
--
-- conversation_kind distinguishes Feishu private/p2p chat ("p2p") from
-- Feishu group chat ("group") and CLI ("cli"). This ensures that an owner's
-- p2p grant is NOT loaded when they send a message in a group chat.

CREATE TABLE IF NOT EXISTS external_operation_grants (
    grant_id                TEXT NOT NULL PRIMARY KEY,
    operation               TEXT NOT NULL,
    grantee_principal_id    TEXT NOT NULL,
    channel                 TEXT NOT NULL,
    conversation_kind       TEXT NOT NULL DEFAULT 'cli'
                            CHECK (conversation_kind IN ('p2p', 'group', 'cli')),
    scope                   TEXT NOT NULL DEFAULT 'principal_channel',
    risk                    TEXT NOT NULL DEFAULT 'Write',
    capability_id           TEXT,
    snapshot_id             TEXT NOT NULL,
    status                  TEXT NOT NULL DEFAULT 'active'
                            CHECK (status IN ('active', 'revoked')),
    created_at              TEXT NOT NULL,
    revoked_at              TEXT,
    created_by_principal_id TEXT,
    decision_reference      TEXT
);

-- Partial unique index: at most one ACTIVE grant per logical tuple.
-- Revoked rows are excluded so the same logical grant can be revoked,
-- re-granted, and revoked again without constraint conflicts.
CREATE UNIQUE INDEX IF NOT EXISTS idx_ext_op_grants_active_unique
    ON external_operation_grants(
         operation, grantee_principal_id, channel, conversation_kind,
         scope, snapshot_id
       )
    WHERE status = 'active';

CREATE INDEX IF NOT EXISTS idx_ext_op_grants_lookup
    ON external_operation_grants(
         grantee_principal_id, channel, conversation_kind, scope, status
       );

CREATE INDEX IF NOT EXISTS idx_ext_op_grants_snapshot
    ON external_operation_grants(snapshot_id);
