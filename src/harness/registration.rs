//! Harness runtime registration — mutable endpoint binding for a bundle hash.
//!
//! PR 2A: only `http` transport, `127.0.0.1` host, no query/fragment.

use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::journal::JournalStore;

/// A validated registration request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeRegistration {
    pub registration_id: String,
    pub bundle_hash: String,
    pub endpoint: String,
    pub transport: String,
    pub enabled: bool,
    pub registered_at: String,
    pub updated_at: String,
}

/// The outcome of a register_runtime call — distinguishes created, updated,
/// and idempotent-unchanged for event writing semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationChange {
    Created(RuntimeRegistration),
    Updated(RuntimeRegistration),
    Unchanged(RuntimeRegistration),
}

impl RegistrationChange {
    pub fn registration(&self) -> &RuntimeRegistration {
        match self {
            RegistrationChange::Created(r)
            | RegistrationChange::Updated(r)
            | RegistrationChange::Unchanged(r) => r,
        }
    }

    pub fn has_changed(&self) -> bool {
        matches!(
            self,
            RegistrationChange::Created(_) | RegistrationChange::Updated(_)
        )
    }
}

/// Register or update a runtime endpoint for a bundle.
/// Uses a single transaction for state write + Journal event.
pub fn register_runtime(
    journal: &JournalStore,
    bundle_hash: &str,
    endpoint: &str,
) -> Result<RuntimeRegistration> {
    let mut conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;

    // Verify bundle exists — inside tx.
    let bundle_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM harness_bundles WHERE bundle_hash = ?1",
        rusqlite::params![bundle_hash],
        |row| row.get(0),
    )?;
    if bundle_count == 0 {
        bail!("bundle_hash not found: {bundle_hash}");
    }

    validate_endpoint(endpoint)?;

    let now = Utc::now().to_rfc3339();

    // Begin transaction.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let change = register_runtime_in_transaction(&tx, bundle_hash, endpoint, &now)?;

    if change.has_changed() {
        let reg = change.registration();
        let action = match &change {
            RegistrationChange::Created(..) => "registered",
            _ => "updated",
        };
        crate::journal::hash_chain::append_event_in_transaction(
            &tx,
            "HarnessRuntimeRegistered",
            &serde_json::to_string(&serde_json::json!({
                "registration_id": reg.registration_id,
                "bundle_hash": bundle_hash,
                "endpoint": endpoint,
                "action": action,
            }))?,
            &now,
        )?;
    }
    // Unchanged: no event needed.

    tx.commit()?;
    drop(conn);

    Ok(change.registration().clone())
}

/// Transaction-aware helper: register or update a runtime endpoint.
/// Does NOT acquire the connection lock or commit. The caller must manage
/// the transaction lifecycle and call append_event_in_transaction for
/// changed registrations.
pub fn register_runtime_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    bundle_hash: &str,
    endpoint: &str,
    now: &str,
) -> Result<RegistrationChange> {
    validate_endpoint(endpoint)?;

    // Check if registration already exists for this bundle_hash.
    let existing: Option<RuntimeRegistration> = tx
        .query_row(
            "SELECT registration_id, bundle_hash, endpoint, transport, enabled, registered_at, updated_at
             FROM harness_runtime_registrations WHERE bundle_hash = ?1",
            rusqlite::params![bundle_hash],
            |row| {
                Ok(RuntimeRegistration {
                    registration_id: row.get(0)?,
                    bundle_hash: row.get(1)?,
                    endpoint: row.get(2)?,
                    transport: row.get(3)?,
                    enabled: row.get::<_, i64>(4)? != 0,
                    registered_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )
        .optional()?;

    if let Some(mut existing_reg) = existing {
        if existing_reg.endpoint == endpoint {
            // Idempotent: same endpoint, unchanged.
            return Ok(RegistrationChange::Unchanged(existing_reg));
        }
        // Update endpoint.
        tx.execute(
            "UPDATE harness_runtime_registrations SET endpoint = ?1, updated_at = ?2 WHERE registration_id = ?3",
            rusqlite::params![endpoint, now, existing_reg.registration_id],
        )?;
        existing_reg.endpoint = endpoint.to_string();
        existing_reg.updated_at = now.to_string();
        Ok(RegistrationChange::Updated(existing_reg))
    } else {
        // Create new registration.
        let reg_id = format!("reg_{}", Uuid::new_v4().simple());
        tx.execute(
            "INSERT INTO harness_runtime_registrations (registration_id, bundle_hash, endpoint, transport, enabled, registered_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
            rusqlite::params![reg_id, bundle_hash, endpoint, "http", now, now],
        )?;
        let reg = RuntimeRegistration {
            registration_id: reg_id,
            bundle_hash: bundle_hash.to_string(),
            endpoint: endpoint.to_string(),
            transport: "http".to_string(),
            enabled: true,
            registered_at: now.to_string(),
            updated_at: now.to_string(),
        };
        Ok(RegistrationChange::Created(reg))
    }
}

/// Validate that the endpoint is localhost-only for v1.
pub fn validate_endpoint(endpoint: &str) -> Result<()> {
    if !endpoint.starts_with("http://127.0.0.1:") {
        bail!("endpoint must be http://127.0.0.1:<port>, got: {endpoint}");
    }
    let port_str = endpoint.trim_start_matches("http://127.0.0.1:");
    if port_str.is_empty() {
        bail!("port must not be empty");
    }
    if port_str.contains('/') || port_str.contains('?') || port_str.contains('#') {
        bail!("port must not contain path, query, or fragment");
    }
    let port: u16 = port_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid port: {port_str}"))?;
    if port == 0 {
        bail!("port must not be 0");
    }
    Ok(())
}

/// List all runtime registrations.
pub fn list_registrations(journal: &JournalStore) -> Result<Vec<RuntimeRegistration>> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let mut stmt = conn.prepare(
        "SELECT registration_id, bundle_hash, endpoint, transport, enabled, registered_at, updated_at
         FROM harness_runtime_registrations ORDER BY registered_at"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RuntimeRegistration {
            registration_id: row.get(0)?,
            bundle_hash: row.get(1)?,
            endpoint: row.get(2)?,
            transport: row.get(3)?,
            enabled: row.get::<_, i64>(4)? != 0,
            registered_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::JournalStore;

    fn in_memory_journal() -> JournalStore {
        JournalStore::in_memory().expect("in-memory journal")
    }

    fn register_bundle(journal: &JournalStore, bundle_id: &str, hash: &str) {
        let conn = journal.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO harness_bundles (bundle_hash, manifest_version, protocol_version, bundle_id, bundle_version, manifest_json, created_at)
             VALUES (?1, 'v1', 'v1', ?2, '1.0', '{}', 'now')",
            rusqlite::params![hash, bundle_id],
        ).unwrap();
    }

    #[test]
    fn validate_endpoint_accepts_localhost() {
        assert!(validate_endpoint("http://127.0.0.1:8080").is_ok());
    }

    #[test]
    fn validate_endpoint_rejects_non_localhost() {
        assert!(validate_endpoint("http://example.com:8080").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_empty_port() {
        assert!(validate_endpoint("http://127.0.0.1:").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_port_zero() {
        assert!(validate_endpoint("http://127.0.0.1:0").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_path_in_port() {
        assert!(validate_endpoint("http://127.0.0.1:8080/path").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_query_in_port() {
        assert!(validate_endpoint("http://127.0.0.1:8080?query=1").is_err());
    }

    #[test]
    fn validate_endpoint_rejects_non_numeric_port() {
        assert!(validate_endpoint("http://127.0.0.1:abcd").is_err());
    }

    #[test]
    fn register_runtime_creates_registration_and_event() {
        let journal = in_memory_journal();
        register_bundle(&journal, "test", "sha256:abc");
        let reg = register_runtime(&journal, "sha256:abc", "http://127.0.0.1:8080").unwrap();
        assert_eq!(reg.bundle_hash, "sha256:abc");
        assert_eq!(reg.endpoint, "http://127.0.0.1:8080");
        // Verify event exists.
        let conn = journal.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM journal_events WHERE kind = 'HarnessRuntimeRegistered'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "event must be created");
    }

    #[test]
    fn register_runtime_update_creates_event() {
        let journal = in_memory_journal();
        register_bundle(&journal, "test", "sha256:abc");
        register_runtime(&journal, "sha256:abc", "http://127.0.0.1:8080").unwrap();
        register_runtime(&journal, "sha256:abc", "http://127.0.0.1:9090").unwrap();
        // Verify endpoint updated.
        let conn = journal.conn.lock().unwrap();
        let endpoint: String = conn.query_row(
            "SELECT endpoint FROM harness_runtime_registrations WHERE bundle_hash = 'sha256:abc'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(endpoint, "http://127.0.0.1:9090");
        // Two events: one for create, one for update.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM journal_events WHERE kind = 'HarnessRuntimeRegistered'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "create + update = 2 events");
    }

    #[test]
    fn register_runtime_idempotent_no_duplicate_event() {
        let journal = in_memory_journal();
        register_bundle(&journal, "test", "sha256:abc");
        register_runtime(&journal, "sha256:abc", "http://127.0.0.1:8080").unwrap();
        register_runtime(&journal, "sha256:abc", "http://127.0.0.1:8080").unwrap();
        // Verify no duplicate event for idempotent call.
        let conn = journal.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM journal_events WHERE kind = 'HarnessRuntimeRegistered'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "idempotent register must not duplicate event");
    }

    #[test]
    fn register_runtime_nonexistent_bundle_fails() {
        let journal = in_memory_journal();
        let err =
            register_runtime(&journal, "sha256:nonexistent", "http://127.0.0.1:8080").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn list_registrations_shows_all() {
        let journal = in_memory_journal();
        register_bundle(&journal, "t1", "sha256:one");
        register_bundle(&journal, "t2", "sha256:two");
        register_runtime(&journal, "sha256:one", "http://127.0.0.1:8080").unwrap();
        register_runtime(&journal, "sha256:two", "http://127.0.0.1:9090").unwrap();
        let regs = list_registrations(&journal).unwrap();
        assert_eq!(regs.len(), 2);
    }

    #[test]
    fn registration_tx_failure_rollback() {
        // Simulate rollback by explicitly dropping the transaction without commit.
        let journal = in_memory_journal();
        register_bundle(&journal, "test", "sha256:abc");

        let mut conn = journal.conn.lock().unwrap();
        validate_endpoint("http://127.0.0.1:8080").unwrap();
        let now = Utc::now().to_rfc3339();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();

        // Register inside tx.
        let result =
            register_runtime_in_transaction(&tx, "sha256:abc", "http://127.0.0.1:8080", &now)
                .unwrap();
        assert!(result.has_changed());

        // Append event successfully.
        let event_payload = serde_json::to_string(&serde_json::json!({
            "registration_id": result.registration().registration_id,
            "bundle_hash": "sha256:abc",
            "endpoint": "http://127.0.0.1:8080",
            "action": "registered",
        }))
        .unwrap();
        let event_result = crate::journal::hash_chain::append_event_in_transaction(
            &tx,
            "HarnessRuntimeRegistered",
            &event_payload,
            &now,
        );
        assert!(event_result.is_ok(), "event append must succeed");

        // Force rollback — don't commit.
        drop(tx);
        drop(conn);

        // Verify no registration was persisted.
        let conn = journal.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM harness_runtime_registrations WHERE bundle_hash = 'sha256:abc'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "registration must not exist after rollback");
    }
}
