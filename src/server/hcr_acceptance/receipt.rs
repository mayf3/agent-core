//! Atomic Receipt append-or-compare with DB-level uniqueness (H3/H6).
//!
//! Uses the `hcr_receipt_identities` table with a UNIQUE constraint on
//! (hcr_id, claim_id, run_id, idempotency_key). All operations happen
//! inside a `BEGIN IMMEDIATE` transaction for cross-connection safety.
//!
//! The `payload_digest` column stores the `receipt_digest` from the
//! `ExternalReceiptEnvelope`. The old `compute_payload_digest()` function
//! is removed — the envelope's receipt_digest provides content integrity.

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{anyhow, Result};
use rusqlite::params;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendReceiptResult {
    Appended,
    Duplicate,
    Conflict(String),
}

/// Append a receipt or detect duplicate/conflict atomically.
///
/// Uses a DB transaction with the UNIQUE constraint on
/// `hcr_receipt_identities(hcr_id, claim_id, run_id, idempotency_key)`.
///
/// Cross-connection safe: the UNIQUE constraint is enforced by SQLite,
/// not by application-level locking.
pub fn append_or_compare_receipt(
    journal: &JournalStore,
    run_id: &RunId,
    session_id: &SessionId,
    hcr_id: &str,
    claim_id: &str,
    the_run_id: &str,
    idempotency_key: &str,
    receipt_status: ReceiptStatus,
    output: &Value,
    payload_digest: &str,
    identity_fields: &ReceiptIdentityFields,
) -> Result<AppendReceiptResult> {
    // Use a transaction for atomicity
    let mut conn = journal.conn.lock().map_err(|e| anyhow!("mutex: {e}"))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Check for existing identity
    let existing: Option<String> = tx
        .query_row(
            "SELECT payload_digest FROM hcr_receipt_identities
         WHERE hcr_id = ?1 AND claim_id = ?2 AND run_id = ?3 AND idempotency_key = ?4",
            params![hcr_id, claim_id, the_run_id, idempotency_key],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(existing_digest) = existing {
        tx.commit()?;
        if existing_digest == payload_digest {
            return Ok(AppendReceiptResult::Duplicate);
        } else {
            return Ok(AppendReceiptResult::Conflict(format!(
                "existing digest {existing_digest} vs new {payload_digest}"
            )));
        }
    }

    // No existing identity — append ReceiptReceived event
    let correlation_id = format!("receipt:{hcr_id}:{claim_id}:{the_run_id}:{idempotency_key}");
    let event_payload = serde_json::json!({
        "invocation_id": format!("hcr_accept_{}", correlation_id),
        "status": format!("{:?}", receipt_status),
        "output": output,
        "correlation_key": correlation_id,
    });

    let event = journal.append_event_in_tx(
        &tx,
        JournalEventKind::ReceiptReceived,
        Some(run_id),
        Some(session_id),
        Some(&correlation_id),
        event_payload,
    )?;

    // Insert receipt identity (UNIQUE constraint protects against races).
    // BOTH the journal event and the identity row are in the same transaction:
    // if either fails, the ENTIRE transaction rolls back — no orphan events.
    let identity_result = tx.execute(
        "INSERT INTO hcr_receipt_identities
         (hcr_id, claim_id, run_id, idempotency_key, payload_digest, receipt_event_id,
          harness_execution_id, overall_outcome, candidate_id, invocation_id, candidate_digest,
          artifact_ref, artifact_digest, delivery_manifest_ref, delivery_manifest_digest,
          evidence_digest, receipt_digest, opaque_payload_digest, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        params![
            hcr_id,
            claim_id,
            the_run_id,
            idempotency_key,
            payload_digest,
            event.event_id.0,
            identity_fields.harness_execution_id,
            identity_fields.overall_outcome,
            identity_fields.candidate_id,
            identity_fields.invocation_id,
            identity_fields.candidate_digest,
            identity_fields.artifact_ref,
            identity_fields.artifact_digest,
            identity_fields.delivery_manifest_ref,
            identity_fields.delivery_manifest_digest,
            identity_fields.evidence_digest,
            identity_fields.receipt_digest,
            identity_fields.opaque_payload_digest,
            chrono::Utc::now().to_rfc3339(),
        ],
    );

    match identity_result {
        Ok(_) => {
            // Both identity and Journal event committed atomically
            tx.commit()?;
            Ok(AppendReceiptResult::Appended)
        }
        Err(e) => {
            // UNIQUE constraint violation: another connection inserted first.
            // ROLLBACK the entire transaction — the Journal event must NOT
            // be committed if the identity row failed. This prevents orphan
            // ReceiptReceived events (H3/H6 fix).
            if let Err(rollback_err) = tx.rollback() {
                // If rollback itself fails, the DB is in a bad state
                return Err(anyhow!(
                    "ROLLBACK_FAILED: {rollback_err} after UNIQUE error: {e}"
                ));
            }

            // Open a fresh transaction to re-read the winner's identity
            let mut conn2 = journal.conn.lock().map_err(|e| anyhow!("mutex: {e}"))?;
            let tx2 = conn2.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

            let winner_digest: Option<String> = tx2
                .query_row(
                    "SELECT payload_digest FROM hcr_receipt_identities
                 WHERE hcr_id = ?1 AND claim_id = ?2 AND run_id = ?3 AND idempotency_key = ?4",
                    params![hcr_id, claim_id, the_run_id, idempotency_key],
                    |row| row.get(0),
                )
                .optional()?;
            tx2.commit()?;

            match winner_digest {
                Some(d) if d == payload_digest => Ok(AppendReceiptResult::Duplicate),
                Some(d) => Ok(AppendReceiptResult::Conflict(format!(
                    "existing {d} vs new {payload_digest}"
                ))),
                None => Ok(AppendReceiptResult::Conflict(
                    "unexpected missing identity after conflict".into(),
                )),
            }
        }
    }
}

/// Fields from the validated acceptance response for receipt identity.
pub struct ReceiptIdentityFields {
    pub harness_execution_id: String,
    pub overall_outcome: String,
    pub candidate_id: String,
    pub invocation_id: String,
    pub candidate_digest: String,
    pub artifact_ref: Option<String>,
    pub artifact_digest: Option<String>,
    /// Delivery manifest content-addressed ref (e.g. "service_manifest_<sha256>").
    pub delivery_manifest_ref: Option<String>,
    /// Delivery manifest ContentStore digest ("sha256:<hex>").
    pub delivery_manifest_digest: Option<String>,
    pub evidence_digest: String,
    /// The receipt_digest from the ExternalReceiptEnvelope.
    pub receipt_digest: String,
    /// The opaque_payload_digest from the ExternalReceiptEnvelope.
    pub opaque_payload_digest: Option<String>,
}

// ── JournalStore extensions ──

impl JournalStore {
    /// Create a persisted Run record for HCR acceptance.
    pub fn create_hcr_run(&self, run: &Run) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow!("mutex: {e}"))?;
        let mode_str = serde_json::to_string(&run.mode)?;
        conn.execute(
            "INSERT OR IGNORE INTO runs (id, session_id, agent_id, trigger_event_id, principal_json,
             parent_run_id, delegated_by, status, created_at, updated_at, registry_snapshot_id, mode)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                run.id.0, run.session_id.0, run.agent_id.0, run.trigger_event_id.0,
                serde_json::to_string(&run.principal)?,
                run.parent_run_id.as_ref().map(|r| r.0.as_str()),
                run.delegated_by.as_ref().map(|p| p.0.as_str()),
                format!("{:?}", run.status),
                run.created_at.to_rfc3339(),
                run.updated_at.to_rfc3339(),
                run.registry_snapshot_id,
                mode_str,
            ],
        )?;
        Ok(())
    }

    /// Append a journal event inside an existing transaction.
    pub fn append_event_in_tx(
        &self,
        tx: &rusqlite::Transaction,
        kind: JournalEventKind,
        run_id: Option<&RunId>,
        session_id: Option<&SessionId>,
        correlation_id: Option<&str>,
        payload: Value,
    ) -> Result<JournalEvent> {
        let event_id = EventId::new();
        let kind_text = format!("{:?}", kind);
        let payload_json = serde_json::to_string(&payload)?;
        let now = chrono::Utc::now().to_rfc3339();

        let prev: Option<(i64, String)> = tx
            .query_row(
                "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let seq = prev.as_ref().map(|(s, _)| s + 1).unwrap_or(1);
        let prev_hash = prev.map(|(_, h)| h);
        let hash = crate::journal::hash_chain::event_hash(
            prev_hash.as_deref(),
            seq,
            &kind_text,
            &payload_json,
        );

        tx.execute(
            "INSERT INTO journal_events (sequence,event_id,run_id,session_id,correlation_id,kind,payload_json,previous_hash,hash,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![seq, event_id.0, run_id.map(|r| r.0.as_str()), session_id.map(|s| s.0.as_str()),
                    correlation_id, kind_text, payload_json, prev_hash, hash, now],
        )?;

        Ok(JournalEvent {
            sequence: seq,
            event_id,
            run_id: run_id.cloned(),
            session_id: session_id.cloned(),
            correlation_id: correlation_id.map(|s| s.to_string()),
            kind,
            payload,
            previous_hash: prev_hash,
            hash,
            created_at: now.parse().unwrap_or_default(),
        })
    }
}

use rusqlite::OptionalExtension;
