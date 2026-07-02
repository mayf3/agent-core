//! Capability change proposal persistence — create, load, decide, query.

use crate::domain::capability_change::*;
use crate::domain::*;
use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::params;

impl super::JournalStore {
    /// Persist a new proposal and write the CapabilityChangeProposed journal event.
    pub fn create_proposal(&self, proposal: &CapabilityChangeProposal) -> Result<String> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        conn.execute(
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
                format!("{:?}", proposal.status),
                proposal.created_at.to_rfc3339(),
                proposal.expires_at.to_rfc3339(),
            ],
        )?;
        self.append_event(
            JournalEventKind::CapabilityChangeProposed,
            Some(&proposal.origin_run_id),
            Some(&proposal.origin_session_id),
            Some(&proposal.proposal_id),
            serde_json::json!({
                "proposal_id": proposal.proposal_id,
                "submitter": proposal.submitter_principal_id,
                "target_agent": proposal.target_agent_id.0,
                "artifact_digest": proposal.artifact_digest,
                "manifest_digest": proposal.manifest_digest,
                "evidence_digest": proposal.evidence_digest,
                "requested_operations": proposal.requested_operations,
                "expected_snapshot_id": proposal.expected_active_snapshot_id,
            }),
        )?;
        Ok(proposal.proposal_id.clone())
    }

    /// Load a proposal by ID, returns None if not found.
    pub fn load_proposal(&self, proposal_id: &str) -> Result<Option<CapabilityChangeProposal>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT proposal_id, submitter_principal_id, target_agent_id,
                    origin_session_id, origin_run_id,
                    artifact_ref, artifact_digest,
                    manifest_ref, manifest_digest,
                    evidence_ref, evidence_digest,
                    requested_operations_json, risk_summary,
                    expected_active_snapshot_id,
                    status, created_at, expires_at,
                    decided_at, decided_by, decision_reason,
                    activated_snapshot_id, activation_error
             FROM capability_change_proposals WHERE proposal_id = ?1"
        )?;
        let mut rows = stmt.query_map(params![proposal_id], |row| {
            Ok(CapabilityChangeProposal {
                proposal_id: row.get(0)?,
                submitter_principal_id: row.get(1)?,
                target_agent_id: AgentId(row.get(2)?),
                origin_session_id: SessionId(row.get(3)?),
                origin_run_id: RunId(row.get(4)?),
                artifact_ref: row.get(5)?,
                artifact_digest: row.get(6)?,
                manifest_ref: row.get(7)?,
                manifest_digest: row.get(8)?,
                evidence_ref: row.get(9)?,
                evidence_digest: row.get(10)?,
                requested_operations: serde_json::from_str(&row.get::<_,String>(11)?).unwrap_or_default(),
                risk_summary: row.get(12)?,
                expected_active_snapshot_id: row.get(13)?,
                status: parse_status(&row.get::<_,String>(14)?),
                created_at: Utc::now(),
                expires_at: Utc::now(),
                decided_at: None,
                decided_by: row.get::<_,Option<String>>(18)?,
                decision_reason: row.get::<_,Option<String>>(19)?,
                activated_snapshot_id: row.get::<_,Option<String>>(20)?,
                activation_error: row.get::<_,Option<String>>(21)?,
            })
        })?;
        match rows.next() {
            Some(Ok(p)) => Ok(Some(p)),
            _ => Ok(None),
        }
    }

    /// Atomically transition a proposal's status. Returns true if the update
    /// affected exactly one row (CAS succeeded).
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
        let current: Option<String> = conn.query_row(
            "SELECT status FROM capability_change_proposals WHERE proposal_id = ?1",
            params![proposal_id], |row| row.get(0),
        ).ok();
        let Some(ref cur) = current else { return Ok(false); };
        if !from_strs.iter().any(|s| s == cur) { return Ok(false); }
        let to = format!("{:?}", to_status);
        let now = Utc::now().to_rfc3339();
        let changed = conn.execute(
            "UPDATE capability_change_proposals
             SET status = ?1, decided_at = ?2, decided_by = ?3, decision_reason = ?4,
                 activated_snapshot_id = ?5, activation_error = ?6
             WHERE proposal_id = ?7 AND status = ?8",
            params![to, now, decided_by, reason, activated_snapshot_id, activation_error, proposal_id, cur],
        )?;
        Ok(changed == 1)
    }

    /// List proposal IDs for a session.
    pub fn proposals_by_session(&self, session_id: &SessionId) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT proposal_id FROM capability_change_proposals WHERE origin_session_id = ?1"
        )?;
        let rows = stmt.query_map(params![session_id.0], |row| row.get::<_,String>(0))?;
        let mut ids = Vec::new();
        for r in rows { ids.push(r?); }
        Ok(ids)
    }
}

fn parse_status(s: &str) -> ProposalStatus {
    match s {
        "PendingApproval" => ProposalStatus::PendingApproval,
        "Approved" => ProposalStatus::Approved,
        "Rejected" => ProposalStatus::Rejected,
        "Activated" => ProposalStatus::Activated,
        "ActivationFailed" => ProposalStatus::ActivationFailed,
        "Expired" => ProposalStatus::Expired,
        _ => ProposalStatus::PendingApproval,
    }
}
