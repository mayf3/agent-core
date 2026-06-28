//! Harness runtime registration — mutable endpoint binding for a bundle hash.
//!
//! PR 2A: only `http` transport, `127.0.0.1` host, no query/fragment.

use anyhow::{bail, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::JournalEventKind;
use crate::journal::JournalStore;

/// A validated registration request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRegistration {
    pub registration_id: String,
    pub bundle_hash: String,
    pub endpoint: String,
    pub transport: String,
    pub enabled: bool,
    pub registered_at: String,
    pub updated_at: String,
}

/// Register or update a runtime endpoint for a bundle.
/// Returns the registration.
pub fn register_runtime(
    journal: &JournalStore,
    bundle_hash: &str,
    endpoint: &str,
) -> Result<RuntimeRegistration> {
    // Verify bundle exists.
    if !bundle_exists(journal, bundle_hash)? {
        bail!("bundle_hash not found: {bundle_hash}");
    }

    validate_endpoint(endpoint)?;

    let transport = "http".to_string();
    let now = Utc::now().to_rfc3339();

    // Check if registration already exists for this bundle_hash.
    let existing = find_by_bundle_hash(journal, bundle_hash)?;

    let (reg_id, is_new) = if let Some(reg) = existing {
        // Update existing.
        update_registration(journal, &reg.registration_id, endpoint, &now)?;
        (reg.registration_id.to_string(), false)
    } else {
        let reg_id = format!("reg_{}", Uuid::new_v4().simple());
        insert_registration(journal, &reg_id, bundle_hash, endpoint, &transport, &now)?;
        (reg_id, true)
    };

    // Write audit event.
    let payload = serde_json::json!({
        "registration_id": reg_id,
        "bundle_hash": bundle_hash,
        "endpoint": endpoint,
        "action": if is_new { "registered" } else { "updated" },
    });
    journal.append_event(
        JournalEventKind::HarnessRuntimeRegistered,
        None,
        None,
        None,
        payload,
    )?;

    Ok(RuntimeRegistration {
        registration_id: reg_id,
        bundle_hash: bundle_hash.to_string(),
        endpoint: endpoint.to_string(),
        transport,
        enabled: true,
        registered_at: now.clone(),
        updated_at: now,
    })
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

// ---- Private DB helpers ----

fn bundle_exists(journal: &JournalStore, bundle_hash: &str) -> Result<bool> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM harness_bundles WHERE bundle_hash = ?1",
        rusqlite::params![bundle_hash],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn find_by_bundle_hash(
    journal: &JournalStore,
    bundle_hash: &str,
) -> Result<Option<RuntimeRegistration>> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    let result = conn.query_row(
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
    );
    match result {
        Ok(reg) => Ok(Some(reg)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn insert_registration(
    journal: &JournalStore,
    reg_id: &str,
    bundle_hash: &str,
    endpoint: &str,
    transport: &str,
    now: &str,
) -> Result<()> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    conn.execute(
        "INSERT INTO harness_runtime_registrations (registration_id, bundle_hash, endpoint, transport, enabled, registered_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
        rusqlite::params![reg_id, bundle_hash, endpoint, transport, now, now],
    )?;
    Ok(())
}

fn update_registration(
    journal: &JournalStore,
    reg_id: &str,
    endpoint: &str,
    now: &str,
) -> Result<()> {
    let conn = journal
        .conn
        .lock()
        .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
    conn.execute(
        "UPDATE harness_runtime_registrations SET endpoint = ?1, updated_at = ?2 WHERE registration_id = ?3",
        rusqlite::params![endpoint, now, reg_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
