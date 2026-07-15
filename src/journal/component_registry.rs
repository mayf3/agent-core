use crate::domain::{
    compute_component_snapshot_id, ComponentRegistrySnapshot, ComponentStatus, RegisteredComponent,
    TargetKind,
};
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

impl super::JournalStore {
    pub(crate) fn initialize_component_registry(&self) -> Result<String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        if let Some(active) = conn
            .query_row(
                "SELECT active_snapshot_id FROM component_registry_state WHERE singleton_id=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            load_snapshot(&conn, &active)?;
            return Ok(active);
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let snapshot_id = persist_snapshot(&tx, &[])?;
        tx.execute(
            "INSERT INTO component_registry_state
             (singleton_id,active_snapshot_id,version,updated_at) VALUES (1,?1,1,?2)",
            params![snapshot_id, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(snapshot_id)
    }

    pub fn current_component_snapshot_id(&self) -> Result<String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT active_snapshot_id FROM component_registry_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn load_component_registry_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<ComponentRegistrySnapshot> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        load_snapshot(&conn, snapshot_id)
    }
}

pub(crate) fn load_snapshot(
    conn: &Connection,
    snapshot_id: &str,
) -> Result<ComponentRegistrySnapshot> {
    let created_at: String = conn
        .query_row(
            "SELECT created_at FROM component_registry_snapshots WHERE snapshot_id=?1",
            params![snapshot_id],
            |row| row.get(0),
        )
        .map_err(|_| anyhow!("COMPONENT_SNAPSHOT_NOT_FOUND"))?;
    let created_at = DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc);
    let mut statement = conn.prepare(
        "SELECT component_id,kind,manifest_id,manifest_digest,artifact_digest,version,endpoint,
                deployment_id,deployment_receipt_id,status,required_contracts_json,
                requested_permissions_json
         FROM component_registry_entries WHERE snapshot_id=?1 ORDER BY component_id",
    )?;
    let rows = statement.query_map(params![snapshot_id], |row| {
        let kind_text: String = row.get(1)?;
        let status_text: String = row.get(9)?;
        Ok(RegisteredComponent {
            component_id: row.get(0)?,
            kind: parse_kind(&kind_text).map_err(to_sql_error)?,
            manifest_id: row.get(2)?,
            manifest_digest: row.get(3)?,
            artifact_digest: row.get(4)?,
            version: row.get(5)?,
            endpoint: row.get(6)?,
            deployment_id: row.get(7)?,
            deployment_receipt_id: row.get(8)?,
            status: parse_status(&status_text).map_err(to_sql_error)?,
            required_contracts: serde_json::from_str(&row.get::<_, String>(10)?)
                .map_err(to_sql_error)?,
            requested_permissions: serde_json::from_str(&row.get::<_, String>(11)?)
                .map_err(to_sql_error)?,
        })
    })?;
    let mut components = Vec::new();
    for row in rows {
        components.push(row?);
    }
    if compute_component_snapshot_id(&components)? != snapshot_id {
        bail!("COMPONENT_SNAPSHOT_DIGEST_MISMATCH");
    }
    Ok(ComponentRegistrySnapshot {
        snapshot_id: snapshot_id.into(),
        created_at,
        components,
    })
}

pub(crate) fn persist_snapshot(
    tx: &Transaction<'_>,
    components: &[RegisteredComponent],
) -> Result<String> {
    let snapshot_id = compute_component_snapshot_id(components)?;
    if tx
        .query_row(
            "SELECT snapshot_id FROM component_registry_snapshots WHERE snapshot_id=?1",
            params![snapshot_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some()
    {
        load_snapshot(tx, &snapshot_id)?;
        return Ok(snapshot_id);
    }
    tx.execute(
        "INSERT INTO component_registry_snapshots
         (snapshot_id,created_at,component_count,canonical_digest) VALUES (?1,?2,?3,?4)",
        params![
            snapshot_id,
            Utc::now().to_rfc3339(),
            components.len() as i64,
            snapshot_id
        ],
    )?;
    let mut sorted = components.to_vec();
    sorted.sort_by(|left, right| left.component_id.cmp(&right.component_id));
    for component in sorted {
        tx.execute(
            "INSERT INTO component_registry_entries
             (snapshot_id,component_id,kind,manifest_id,manifest_digest,artifact_digest,version,
              endpoint,deployment_id,deployment_receipt_id,status,required_contracts_json,
              requested_permissions_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                snapshot_id,
                component.component_id,
                kind_text(component.kind),
                component.manifest_id,
                component.manifest_digest,
                component.artifact_digest,
                component.version,
                component.endpoint,
                component.deployment_id,
                component.deployment_receipt_id,
                status_text(&component.status),
                serde_json::to_string(&component.required_contracts)?,
                serde_json::to_string(&component.requested_permissions)?,
            ],
        )?;
    }
    Ok(snapshot_id)
}

fn kind_text(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::InvocableCapability => "invocable_capability",
        TargetKind::HookConsumerService => "hook_consumer_service",
        TargetKind::ContextProvider => "context_provider",
        TargetKind::ContextTransformer => "context_transformer",
        TargetKind::ScheduledWorker => "scheduled_worker",
        TargetKind::SchedulerService => "scheduler_service",
        TargetKind::IngressRouter => "ingress_router",
        TargetKind::MultiRunOrchestrator => "multi_run_orchestrator",
        TargetKind::ConnectorExtension => "connector_extension",
    }
}

fn parse_kind(value: &str) -> Result<TargetKind> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(Into::into)
}

fn status_text(status: &ComponentStatus) -> &'static str {
    match status {
        ComponentStatus::Healthy => "healthy",
        ComponentStatus::Disabled => "disabled",
        ComponentStatus::RolledBack => "rolled_back",
    }
}

fn parse_status(value: &str) -> Result<ComponentStatus> {
    match value {
        "healthy" => Ok(ComponentStatus::Healthy),
        "disabled" => Ok(ComponentStatus::Disabled),
        "rolled_back" => Ok(ComponentStatus::RolledBack),
        _ => bail!("COMPONENT_STATUS_INVALID"),
    }
}

fn to_sql_error(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_component_snapshot_is_stable_across_restart() {
        let journal = super::super::JournalStore::in_memory().unwrap();
        let first = journal.current_component_snapshot_id().unwrap();
        let second = journal.initialize_component_registry().unwrap();
        assert_eq!(first, second);
        assert!(journal
            .load_component_registry_snapshot(&first)
            .unwrap()
            .components
            .is_empty());
    }
}
