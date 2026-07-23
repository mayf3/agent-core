//! Strict proposal persistence — strict parsing, atomic journal transactions.

use crate::domain::capability_change::*;
use crate::domain::*;
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

#[allow(dead_code)]
fn parse_status(s: &str) -> Result<ProposalStatus> {
    match s {
        "PendingApproval" => Ok(ProposalStatus::PendingApproval),
        "Approved" => Ok(ProposalStatus::Approved),
        "Rejected" => Ok(ProposalStatus::Rejected),
        "Activated" => Ok(ProposalStatus::Activated),
        "ActivationFailed" => Ok(ProposalStatus::ActivationFailed),
        "Expired" => Ok(ProposalStatus::Expired),
        o => bail!("unknown_proposal_status:{o}"),
    }
}

#[allow(dead_code)]
fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| anyhow!("invalid_timestamp:{e}"))
}

#[allow(dead_code)]
fn parse_ops(s: &str) -> Result<Vec<String>> {
    serde_json::from_str(s).map_err(|e| anyhow!("invalid_operations_json:{e}"))
}

impl super::JournalStore {
    /// Return the pending Proposal created by a Run, if any. The unique HCR
    /// settlement path permits at most one proposal per operation and Run;
    /// ordering makes recovery deterministic if future workflows add more.
    pub fn pending_capability_proposal_for_run(&self, run_id: &RunId) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        conn.query_row(
            "SELECT proposal_id FROM capability_change_proposals
             WHERE origin_run_id=?1 AND status='PendingApproval'
             ORDER BY created_at DESC, proposal_id DESC LIMIT 1",
            params![run_id.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Return the authoritative channel and conversation kind of the Run that
    /// created a Proposal. Private Feishu sessions use an open_id key; group
    /// sessions use a chat_id key.
    pub fn load_proposal_origin_context(
        &self,
        proposal_id: &str,
    ) -> Result<Option<(String, String)>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        conn.query_row(
            "SELECT s.channel,s.conversation_key
             FROM capability_change_proposals p
             JOIN sessions s ON s.id=p.origin_session_id
             WHERE p.proposal_id=?1",
            params![proposal_id],
            |row| {
                let channel: String = row.get(0)?;
                let key: String = row.get(1)?;
                let kind = if channel == "Feishu" && key.starts_with("feishu:open_id:") {
                    "p2p"
                } else if channel == "Feishu" && key.starts_with("feishu:chat_id:") {
                    "group"
                } else {
                    "unknown"
                };
                Ok((channel, kind.to_string()))
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_proposal(&self, proposal: &CapabilityChangeProposal) -> Result<String> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO capability_change_proposals
             (proposal_id, submitter_principal_id, target_agent_id,
              origin_session_id, origin_run_id,
              artifact_ref, artifact_digest, manifest_ref, manifest_digest,
              evidence_ref, evidence_digest,
              requested_operations_json, risk_summary, expected_active_snapshot_id,
              status, created_at, expires_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                proposal.proposal_id,
                proposal.submitter_principal_id,
                proposal.target_agent_id.0,
                proposal.origin_session_id.0,
                proposal.origin_run_id.0,
                proposal.artifact_ref,
                proposal.artifact_digest,
                proposal.manifest_ref,
                proposal.manifest_digest,
                proposal.evidence_ref,
                proposal.evidence_digest,
                serde_json::to_string(&proposal.requested_operations)?,
                proposal.risk_summary,
                proposal.expected_active_snapshot_id,
                "PendingApproval",
                proposal.created_at.to_rfc3339(),
                proposal.expires_at.to_rfc3339(),
            ],
        )?;
        super::queue::append_event_tx(
            &tx,
            JournalEventKind::CapabilityChangeProposed,
            Some(&proposal.origin_run_id),
            Some(&proposal.origin_session_id),
            Some(&proposal.proposal_id),
            serde_json::json!({
                "proposal_id": proposal.proposal_id,
                "submitter": proposal.submitter_principal_id,
                "artifact_digest": proposal.artifact_digest,
                "manifest_digest": proposal.manifest_digest,
                "requested_operations": proposal.requested_operations,
                "expected_snapshot_id": proposal.expected_active_snapshot_id,
            }),
        )?;
        tx.commit()?;
        Ok(proposal.proposal_id.clone())
    }

    pub fn load_proposal(&self, proposal_id: &str) -> Result<Option<CapabilityChangeProposal>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT proposal_id, submitter_principal_id, target_agent_id,
                    origin_session_id, origin_run_id,
                    artifact_ref, artifact_digest, manifest_ref, manifest_digest,
                    evidence_ref, evidence_digest,
                    requested_operations_json, risk_summary,
                    expected_active_snapshot_id,
                    status, created_at, expires_at,
                    decided_at, decided_by, decision_reason,
                    activated_snapshot_id, activation_error
             FROM capability_change_proposals WHERE proposal_id = ?1",
        )?;
        let row: std::result::Result<CapabilityChangeProposal, rusqlite::Error> =
            stmt.query_row(params![proposal_id], |row| {
                let g = |i: usize| -> std::result::Result<String, rusqlite::Error> { row.get(i) };
                let go = |i: usize| -> std::result::Result<Option<String>, rusqlite::Error> {
                    row.get(i)
                };
                Ok(CapabilityChangeProposal {
                    proposal_id: g(0)?,
                    submitter_principal_id: g(1)?,
                    target_agent_id: AgentId(g(2)?),
                    origin_session_id: SessionId(g(3)?),
                    origin_run_id: RunId(g(4)?),
                    artifact_ref: g(5)?,
                    artifact_digest: g(6)?,
                    manifest_ref: g(7)?,
                    manifest_digest: g(8)?,
                    evidence_ref: g(9)?,
                    evidence_digest: g(10)?,
                    requested_operations: serde_json::from_str(&g(11)?)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    risk_summary: g(12)?,
                    expected_active_snapshot_id: g(13)?,
                    status: match g(14)?.as_str() {
                        "PendingApproval" => Ok(ProposalStatus::PendingApproval),
                        "Approved" => Ok(ProposalStatus::Approved),
                        "Rejected" => Ok(ProposalStatus::Rejected),
                        "Activated" => Ok(ProposalStatus::Activated),
                        "ActivationFailed" => Ok(ProposalStatus::ActivationFailed),
                        "Expired" => Ok(ProposalStatus::Expired),
                        o => Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unknown_status:{o}"),
                            ),
                        ))),
                    }?,
                    created_at: parse_ts(&g(15)?).map_err(|e| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("invalid_created_at:{e}"),
                        )))
                    })?,
                    expires_at: parse_ts(&g(16)?).map_err(|e| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("invalid_expires_at:{e}"),
                        )))
                    })?,
                    decided_at: match go(17)? {
                        Some(s) => Some(parse_ts(&s).map_err(|e| {
                            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("invalid_decided_at:{e}"),
                            )))
                        })?),
                        None => None,
                    },
                    decided_by: go(18)?,
                    decision_reason: go(19)?,
                    activated_snapshot_id: go(20)?,
                    activation_error: go(21)?,
                })
            });
        match row {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => bail!("proposal_decode_failed:{e}"),
        }
    }

    pub fn decide_proposal(
        &self,
        proposal_id: &str,
        from_status: &[ProposalStatus],
        to_status: ProposalStatus,
        decided_by: &str,
        reason: &str,
        activated_snapshot_id: Option<&str>,
        activation_error: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let from_strs: Vec<String> = from_status.iter().map(|s| format!("{:?}", s)).collect();
        let current: Option<String> = conn
            .query_row(
                "SELECT status FROM capability_change_proposals WHERE proposal_id = ?1",
                params![proposal_id],
                |row| row.get(0),
            )
            .ok();
        let Some(ref cur) = current else {
            return Ok(false);
        };
        if !from_strs.iter().any(|s| s == cur) {
            return Ok(false);
        }
        let to = format!("{:?}", to_status);
        let now = Utc::now().to_rfc3339();
        let changed = conn.execute(
            "UPDATE capability_change_proposals
             SET status = ?1, decided_at = ?2, decided_by = ?3, decision_reason = ?4,
                 activated_snapshot_id = ?5, activation_error = ?6
             WHERE proposal_id = ?7 AND status = ?8",
            params![
                to,
                now,
                decided_by,
                reason,
                activated_snapshot_id,
                activation_error,
                proposal_id,
                cur
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn proposals_by_session(&self, session_id: &SessionId) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT proposal_id FROM capability_change_proposals WHERE origin_session_id = ?1",
        )?;
        let rows = stmt.query_map(params![session_id.0], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for r in rows {
            ids.push(r?);
        }
        Ok(ids)
    }

    /// Atomically reject a PendingApproval proposal: CAS-update status to
    /// Rejected and append the `CapabilityChangeRejected` event in a single
    /// transaction. On failure (not found, not Pending) the transaction rolls
    /// back and the proposal state is unchanged.
    pub fn reject_proposal_atomic(
        &self,
        proposal_id: &str,
        decided_by: &str,
        reason: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let cur: Option<String> = tx
            .query_row(
                "SELECT status FROM capability_change_proposals WHERE proposal_id = ?1",
                params![proposal_id],
                |row| row.get(0),
            )
            .ok();
        match cur.as_deref() {
            Some("PendingApproval") => {}
            Some(s) => bail!("proposal_not_pending:{s}"),
            None => bail!("proposal_not_found"),
        }
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE capability_change_proposals SET status = 'Rejected', decided_at = ?1, decided_by = ?2, decision_reason = ?3 WHERE proposal_id = ?4 AND status = 'PendingApproval'",
            params![now, decided_by, reason, proposal_id],
        )?;
        if changed != 1 {
            bail!("proposal_not_pending");
        }
        super::queue::append_event_tx(
            &tx,
            JournalEventKind::CapabilityChangeRejected,
            None,
            None,
            Some(proposal_id),
            serde_json::json!({"proposal_id": proposal_id, "decided_by": decided_by, "reason": reason}),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically expire a PendingApproval proposal: CAS-update status to
    /// Expired and append the `CapabilityChangeExpired` event in a single
    /// transaction. On failure (not found, not Pending) the transaction rolls
    /// back and the proposal state is unchanged.
    pub fn expire_proposal_atomic(
        &self,
        proposal_id: &str,
        decided_by: &str,
        reason: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let cur: Option<String> = tx
            .query_row(
                "SELECT status FROM capability_change_proposals WHERE proposal_id = ?1",
                params![proposal_id],
                |row| row.get(0),
            )
            .ok();
        match cur.as_deref() {
            Some("PendingApproval") => {}
            Some(s) => bail!("proposal_not_pending:{s}"),
            None => bail!("proposal_not_found"),
        }
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE capability_change_proposals SET status = 'Expired', decided_at = ?1, decided_by = ?2, decision_reason = ?3 WHERE proposal_id = ?4 AND status = 'PendingApproval'",
            params![now, decided_by, reason, proposal_id],
        )?;
        if changed != 1 {
            bail!("proposal_not_pending");
        }
        super::queue::append_event_tx(
            &tx,
            JournalEventKind::CapabilityChangeExpired,
            None,
            None,
            Some(proposal_id),
            serde_json::json!({"proposal_id": proposal_id, "decided_by": decided_by, "reason": reason}),
        )?;
        tx.commit()?;
        Ok(())
    }
}
