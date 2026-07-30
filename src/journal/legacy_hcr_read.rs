//! Read-only queries for retained historical HCR tables.

use crate::domain::HcrSettlement;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

impl super::JournalStore {
    /// Retained settlement lookup for migration verification and legacy
    /// read-only consumers. This method never mutates HCR state.
    pub fn get_settlement(&self, hcr_id: &str) -> Result<Option<HcrSettlement>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT settlement_id,hcr_id,claim_id,run_id,result,error_code,
                    evidence_set_digest,failure_evidence_event_id,created_at
             FROM hcr_settlements WHERE hcr_id=?1",
            params![hcr_id],
            |row| {
                Ok(HcrSettlement {
                    settlement_id: row.get(0)?,
                    hcr_id: row.get(1)?,
                    claim_id: row.get(2)?,
                    run_id: row.get(3)?,
                    result: row.get(4)?,
                    error_code: row.get(5)?,
                    evidence_set_digest: row.get(6)?,
                    failure_evidence_event_id: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn load_legacy_hcr_snapshot(&self, hcr_id: &str) -> Result<Option<Value>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let request = conn
            .query_row(
                "SELECT request_id,source,source_message_id,session_id,principal_id,
                        channel,chat_type,harness_id,requirement,status,created_at,
                        updated_at,run_id,error_code
                 FROM harness_change_requests WHERE request_id=?1",
                params![hcr_id],
                |row| {
                    Ok(json!({
                        "request_id": row.get::<_, String>(0)?,
                        "source": row.get::<_, String>(1)?,
                        "source_message_id": row.get::<_, String>(2)?,
                        "session_id": row.get::<_, String>(3)?,
                        "principal_id": row.get::<_, String>(4)?,
                        "channel": row.get::<_, String>(5)?,
                        "chat_type": row.get::<_, String>(6)?,
                        "harness_id": row.get::<_, String>(7)?,
                        "requirement": row.get::<_, String>(8)?,
                        "status": row.get::<_, String>(9)?,
                        "created_at": row.get::<_, String>(10)?,
                        "updated_at": row.get::<_, String>(11)?,
                        "run_id": row.get::<_, Option<String>>(12)?,
                        "error_code": row.get::<_, Option<String>>(13)?,
                    }))
                },
            )
            .optional()?;
        let Some(request) = request else {
            return Ok(None);
        };

        let claim = conn
            .query_row(
                "SELECT claim_id,harness_id,worker_instance_id,claimed_at,status
                 FROM hcr_claims WHERE hcr_id=?1",
                params![hcr_id],
                |row| {
                    Ok(json!({
                        "claim_id": row.get::<_, String>(0)?,
                        "harness_id": row.get::<_, String>(1)?,
                        "worker_instance_id": row.get::<_, String>(2)?,
                        "claimed_at": row.get::<_, String>(3)?,
                        "status": row.get::<_, String>(4)?,
                    }))
                },
            )
            .optional()?;
        let settlement = conn
            .query_row(
                "SELECT settlement_id,claim_id,run_id,result,error_code,
                        evidence_set_digest,failure_evidence_event_id,created_at
                 FROM hcr_settlements WHERE hcr_id=?1",
                params![hcr_id],
                |row| {
                    Ok(json!({
                        "settlement_id": row.get::<_, String>(0)?,
                        "claim_id": row.get::<_, String>(1)?,
                        "run_id": row.get::<_, String>(2)?,
                        "result": row.get::<_, String>(3)?,
                        "error_code": row.get::<_, Option<String>>(4)?,
                        "evidence_set_digest": row.get::<_, String>(5)?,
                        "failure_evidence_event_id": row.get::<_, Option<String>>(6)?,
                        "created_at": row.get::<_, String>(7)?,
                    }))
                },
            )
            .optional()?;
        let gate_attempt_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM hcr_gate_attempts WHERE hcr_id=?1",
            params![hcr_id],
            |row| row.get(0),
        )?;
        let gate_evidence_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM hcr_gate_evidence e
             JOIN hcr_gate_attempts a ON a.gate_attempt_id=e.gate_attempt_id
             WHERE a.hcr_id=?1",
            params![hcr_id],
            |row| row.get(0),
        )?;
        Ok(Some(json!({
            "request": request,
            "claim": claim,
            "settlement": settlement,
            "gate_attempt_count": gate_attempt_count,
            "gate_evidence_count": gate_evidence_count,
            "read_only": true,
        })))
    }
}
