//! External operation grant CRUD operations.
//!
//! Separates grant authorization from capability activation: activating a
//! capability only registers the operation in the registry snapshot; a grant
//! is required for a specific principal to invoke it.  Grants are persisted
//! in the `external_operation_grants` table and loaded during Run creation.
//!
//! # Idempotency
//!
//! `create` uses INSERT OR IGNORE with a partial unique index on
//! (operation, grantee_principal_id, channel, scope, snapshot_id)
//! WHERE status = 'active' so repeated calls with the same active tuple
//! are safe. On duplicate, the existing persistent grant_id is returned
//! (not a freshly generated UUID that never reached the database).
//!
//! `revoke` on an already-revoked grant is a no-op that returns Ok(()).
//!
//! # Journal events
//!
//! `create` appends `ExternalOperationGranted` ONLY when a new row is
//! actually inserted. Idempotent (duplicate) creates do NOT emit a second
//! event. `revoke` appends `ExternalOperationRevoked` ONLY on a genuine
//! active → revoked transition.

use crate::domain::*;
use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

/// Parameters for creating a new external operation grant.
///
/// `conversation_kind` must be one of "p2p" (Feishu private chat),
/// "group" (Feishu group chat), or "cli" (CLI). The runtime derives this
/// from `ValidatedEvent.chat_type` and `session.channel` during Run creation.
pub struct CreateGrantParams {
    pub operation: String,
    pub grantee_principal_id: String,
    pub channel: String,
    pub conversation_kind: String,
    pub scope: String,
    pub risk: String,
    pub capability_id: Option<String>,
    pub snapshot_id: String,
    pub created_by_principal_id: Option<String>,
    pub decision_reference: Option<String>,
}

impl super::JournalStore {
    /// Create an external operation grant. Idempotent: if a matching active
    /// grant already exists (same operation + principal + channel + scope +
    /// snapshot), returns the EXISTING persistent grant_id (not a fake UUID).
    ///
    /// Appends an `ExternalOperationGranted` journal event ONLY on actual
    /// insert. Duplicate creates do not emit a second event.
    pub fn create_external_operation_grant(&self, params: CreateGrantParams) -> Result<String> {
        let grant_id = format!("grt_{}", Uuid::new_v4());
        let now = Utc::now().to_rfc3339();

        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;

        let rows_affected = conn.execute(
            "INSERT OR IGNORE INTO external_operation_grants
             (grant_id, operation, grantee_principal_id, channel, conversation_kind,
              scope, risk, capability_id, snapshot_id, status, created_at,
              created_by_principal_id, decision_reference)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', ?10, ?11, ?12)",
            params![
                grant_id,
                params.operation,
                params.grantee_principal_id,
                params.channel,
                params.conversation_kind,
                params.scope,
                params.risk,
                params.capability_id,
                params.snapshot_id,
                now,
                params.created_by_principal_id,
                params.decision_reference,
            ],
        )?;

        // Determine the real persistent grant_id.
        let persisted_grant_id = if rows_affected == 0 {
            // INSERT was a no-op (duplicate active grant exists).
            // Query the existing grant_id from the DB.
            let existing: String = conn.query_row(
                "SELECT grant_id FROM external_operation_grants
                 WHERE operation = ?1 AND grantee_principal_id = ?2
                   AND channel = ?3 AND conversation_kind = ?4
                   AND scope = ?5 AND snapshot_id = ?6
                   AND status = 'active'",
                params![
                    params.operation,
                    params.grantee_principal_id,
                    params.channel,
                    params.conversation_kind,
                    params.scope,
                    params.snapshot_id,
                ],
                |row| row.get(0),
            )?;
            existing
        } else {
            grant_id
        };

        drop(conn);

        // Only emit journal event on actual insert.
        if rows_affected > 0 {
            self.append_event(
                JournalEventKind::ExternalOperationGranted,
                None,
                None,
                Some(&persisted_grant_id),
                serde_json::json!({
                    "grant_id": persisted_grant_id,
                    "operation": params.operation,
                    "grantee_principal_id": params.grantee_principal_id,
                    "channel": params.channel,
                    "conversation_kind": params.conversation_kind,
                    "scope": params.scope,
                    "risk": params.risk,
                    "snapshot_id": params.snapshot_id,
                }),
            )?;
        }

        Ok(persisted_grant_id)
    }

    /// Revoke an external operation grant. Idempotent: if the grant is already
    /// revoked or does not exist, this is a no-op that returns `Ok(())`.
    ///
    /// Appends an `ExternalOperationRevoked` journal event when a genuine
    /// revocation occurs (active → revoked transition).
    pub fn revoke_external_operation_grant(&self, grant_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;

        let changed = conn.execute(
            "UPDATE external_operation_grants
             SET status = 'revoked', revoked_at = ?1
             WHERE grant_id = ?2 AND status = 'active'",
            params![now, grant_id],
        )?;

        drop(conn);

        // Only emit the event if a row was genuinely transitioned.
        if changed > 0 {
            self.append_event(
                JournalEventKind::ExternalOperationRevoked,
                None,
                None,
                Some(grant_id),
                serde_json::json!({
                    "grant_id": grant_id,
                    "revoked_at": now,
                }),
            )?;
        }

        Ok(())
    }

    /// Load all active external operation grants that match the given
    /// principal, channel, conversation_kind, scope, and registry snapshot.
    ///
    /// Only grants with `status = 'active'` are returned. Revoked grants,
    /// grants for a different principal/channel/kint/scope/snapshot are excluded.
    pub fn load_active_external_operation_grants(
        &self,
        principal_id: &str,
        channel: &str,
        conversation_kind: &str,
        scope: &str,
        snapshot_id: &str,
    ) -> Result<Vec<ExternalOperationGrant>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;

        let mut stmt = conn.prepare(
            "SELECT grant_id, operation, grantee_principal_id, channel,
                    conversation_kind, scope, risk, snapshot_id, status
             FROM external_operation_grants
             WHERE status = 'active'
               AND grantee_principal_id = ?1
               AND channel = ?2
               AND conversation_kind = ?3
               AND scope = ?4
               AND snapshot_id = ?5",
        )?;

        let rows = stmt.query_map(
            params![principal_id, channel, conversation_kind, scope, snapshot_id],
            |row| {
                Ok(ExternalOperationGrant {
                    grant_id: row.get(0)?,
                    operation: row.get(1)?,
                    grantee_principal_id: row.get(2)?,
                    channel: row.get(3)?,
                    conversation_kind: row.get(4)?,
                    scope: row.get(5)?,
                    risk: row.get(6)?,
                    snapshot_id: row.get(7)?,
                    status: row.get(8)?,
                })
            },
        )?;

        let mut grants = Vec::new();
        for row in rows {
            grants.push(row?);
        }

        Ok(grants)
    }
}
