//! Authoritative HCR-derived Capability Proposal persistence.

use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl super::JournalStore {
    /// Validate every trusted field from authoritative rows, then atomically
    /// persist the Proposal, link and Journal fact.
    pub fn create_proposal_with_hcr_link(
        &self,
        proposal: &CapabilityChangeProposal,
        link: &CapabilityProposalHcrLink,
    ) -> Result<String> {
        validate_caller_fields(proposal, link)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing) = tx
            .query_row(
                "SELECT proposal_id FROM capability_proposal_hcr_links
                 WHERE hcr_id=?1 AND candidate_digest=?2 AND operation=?3",
                params![link.hcr_id, link.candidate_digest, link.operation],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            tx.commit()?;
            return Ok(existing);
        }

        let (hcr_status, hcr_session, hcr_principal): (String, String, String) = tx
            .query_row(
                "SELECT status, session_id, principal_id FROM harness_change_requests
                 WHERE request_id=?1",
                params![link.hcr_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow::anyhow!("TRUSTED_HCR_NOT_FOUND"))?;
        if hcr_status != "succeeded"
            || hcr_session != proposal.origin_session_id.0
            || hcr_principal != proposal.submitter_principal_id
        {
            bail!("TRUSTED_HCR_ORIGIN_MISMATCH");
        }

        let settlement: (String, String, String, String) = tx
            .query_row(
                "SELECT claim_id, run_id, result, evidence_set_digest
                 FROM hcr_settlements WHERE settlement_id=?1 AND hcr_id=?2",
                params![link.settlement_id, link.hcr_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|_| anyhow::anyhow!("TRUSTED_SETTLEMENT_NOT_FOUND"))?;
        if settlement.0 != link.claim_id
            || settlement.1 != link.run_id
            || settlement.2 != "succeeded"
        {
            bail!("TRUSTED_SETTLEMENT_MISMATCH");
        }

        let attempts: i64 = tx.query_row(
            "SELECT COUNT(*) FROM hcr_gate_attempts
             WHERE hcr_id=?1 AND claim_id=?2 AND run_id=?3",
            params![link.hcr_id, link.claim_id, link.run_id],
            |row| row.get(0),
        )?;
        let evidence: i64 = tx.query_row(
            "SELECT COUNT(*) FROM hcr_gate_evidence e
             JOIN hcr_gate_attempts a ON a.gate_attempt_id=e.gate_attempt_id
             WHERE a.hcr_id=?1 AND a.claim_id=?2 AND a.run_id=?3",
            params![link.hcr_id, link.claim_id, link.run_id],
            |row| row.get(0),
        )?;
        if attempts != 5 || evidence != 5 {
            bail!("TRUSTED_GATE_SET_INCOMPLETE");
        }

        let receipt: (i64, String, String, String, String, String, String, String) = tx
            .query_row(
                "SELECT COUNT(*), overall_outcome, candidate_id, candidate_digest,
                        artifact_ref, artifact_digest, evidence_digest, invocation_id
                 FROM hcr_receipt_identities
                 WHERE hcr_id=?1 AND claim_id=?2 AND run_id=?3",
                params![link.hcr_id, link.claim_id, link.run_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .map_err(|_| anyhow::anyhow!("TRUSTED_RECEIPT_NOT_FOUND"))?;
        if receipt.0 != 1
            || receipt.1 != "CandidatePassed"
            || receipt.2 != link.candidate_id
            || receipt.3 != link.candidate_digest
            || receipt.4 != link.artifact_ref
            || receipt.5 != link.artifact_digest
            || receipt.6 != link.evidence_digest
            || receipt.7.is_empty()
        {
            bail!("TRUSTED_RECEIPT_MISMATCH");
        }

        let (origin_session, origin_snapshot, principal_json): (String, String, String) = tx
            .query_row(
                "SELECT session_id, registry_snapshot_id, principal_json FROM runs WHERE id=?1",
                params![proposal.origin_run_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow::anyhow!("PROPOSAL_ORIGIN_RUN_NOT_FOUND"))?;
        let principal: RunPrincipal = serde_json::from_str(&principal_json)?;
        if origin_session != proposal.origin_session_id.0
            || origin_snapshot != link.source_registry_snapshot_id
            || principal.principal_id.0 != proposal.submitter_principal_id
        {
            bail!("PROPOSAL_ORIGIN_RUN_MISMATCH");
        }
        let active: String = tx.query_row(
            "SELECT active_snapshot_id FROM registry_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )?;
        if active != link.source_registry_snapshot_id {
            bail!("SOURCE_REGISTRY_SNAPSHOT_CHANGED");
        }

        insert_proposal(&tx, proposal)?;
        tx.execute(
            "INSERT INTO capability_proposal_hcr_links
             (proposal_id,hcr_id,claim_id,run_id,operation,candidate_id,candidate_digest,
              artifact_ref,artifact_digest,evidence_digest,source_registry_snapshot_id,
              settlement_id,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                link.proposal_id,
                link.hcr_id,
                link.claim_id,
                link.run_id,
                link.operation,
                link.candidate_id,
                link.candidate_digest,
                link.artifact_ref,
                link.artifact_digest,
                link.evidence_digest,
                link.source_registry_snapshot_id,
                link.settlement_id,
                link.created_at,
            ],
        )?;
        let approval_id = format!("approval_{}", uuid::Uuid::new_v4().simple());
        let decision_nonce = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        tx.execute(
            "INSERT INTO capability_change_approvals
             (approval_id,proposal_id,owner_principal_id,source_registry_snapshot_id,
              candidate_digest,artifact_digest,manifest_digest,decision_nonce,status,
              created_at,expires_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'Pending',?9,?10)",
            params![
                approval_id,
                proposal.proposal_id,
                proposal.submitter_principal_id,
                link.source_registry_snapshot_id,
                link.candidate_digest,
                link.artifact_digest,
                proposal.manifest_digest,
                decision_nonce,
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
                "hcr_id": link.hcr_id,
                "candidate_id": link.candidate_id,
                "candidate_digest": link.candidate_digest,
                "settlement_id": link.settlement_id,
                "approval_id": approval_id,
                "approval_expires_at": proposal.expires_at,
            }),
        )?;
        tx.commit()?;
        Ok(proposal.proposal_id.clone())
    }

    pub fn load_proposal_hcr_link(
        &self,
        proposal_id: &str,
    ) -> Result<Option<CapabilityProposalHcrLink>> {
        self.load_proposal_hcr_link_where("proposal_id", proposal_id)
    }

    pub fn load_proposal_hcr_link_by_hcr(
        &self,
        hcr_id: &str,
    ) -> Result<Option<CapabilityProposalHcrLink>> {
        self.load_proposal_hcr_link_where("hcr_id", hcr_id)
    }

    fn load_proposal_hcr_link_where(
        &self,
        column: &str,
        value: &str,
    ) -> Result<Option<CapabilityProposalHcrLink>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let sql = format!(
            "SELECT proposal_id,hcr_id,claim_id,run_id,operation,candidate_id,candidate_digest,
                    artifact_ref,artifact_digest,evidence_digest,source_registry_snapshot_id,
                    settlement_id,created_at FROM capability_proposal_hcr_links WHERE {column}=?1"
        );
        conn.query_row(&sql, params![value], row_to_link)
            .optional()
            .map_err(Into::into)
    }

    pub fn load_hcr_receipt_identity(&self, hcr_id: &str) -> Result<Option<(String, String)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        conn.query_row(
            "SELECT invocation_id,harness_execution_id FROM hcr_receipt_identities
             WHERE hcr_id=?1",
            params![hcr_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }
}

fn validate_caller_fields(
    proposal: &CapabilityChangeProposal,
    link: &CapabilityProposalHcrLink,
) -> Result<()> {
    if proposal.proposal_id != link.proposal_id
        || proposal.artifact_ref != link.artifact_ref
        || proposal.artifact_digest != link.artifact_digest
        || proposal.evidence_digest != link.evidence_digest
        || proposal.expected_active_snapshot_id != link.source_registry_snapshot_id
        || proposal.requested_operations != [link.operation.clone()]
        || link.operation != "external.calculator"
    {
        bail!("PROPOSAL_LINK_FIELD_MISMATCH");
    }
    for value in [
        &link.proposal_id,
        &link.hcr_id,
        &link.claim_id,
        &link.run_id,
        &link.candidate_id,
        &link.settlement_id,
    ] {
        if value.trim().is_empty() {
            bail!("PROPOSAL_LINK_EMPTY_FIELD");
        }
    }
    for digest in [
        &link.candidate_digest,
        &link.artifact_ref,
        &link.artifact_digest,
        &link.evidence_digest,
        &proposal.manifest_digest,
    ] {
        crate::capabilities::store::Sha256Digest::parse(digest)?;
    }
    Ok(())
}

fn insert_proposal(
    tx: &rusqlite::Transaction<'_>,
    proposal: &CapabilityChangeProposal,
) -> Result<()> {
    tx.execute(
        "INSERT INTO capability_change_proposals
         (proposal_id,submitter_principal_id,target_agent_id,origin_session_id,origin_run_id,
          artifact_ref,artifact_digest,manifest_ref,manifest_digest,evidence_ref,evidence_digest,
          requested_operations_json,risk_summary,expected_active_snapshot_id,status,created_at,expires_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'PendingApproval',?15,?16)",
        params![
            proposal.proposal_id, proposal.submitter_principal_id, proposal.target_agent_id.0,
            proposal.origin_session_id.0, proposal.origin_run_id.0, proposal.artifact_ref,
            proposal.artifact_digest, proposal.manifest_ref, proposal.manifest_digest,
            proposal.evidence_ref, proposal.evidence_digest,
            serde_json::to_string(&proposal.requested_operations)?, proposal.risk_summary,
            proposal.expected_active_snapshot_id, proposal.created_at.to_rfc3339(),
            proposal.expires_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn row_to_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<CapabilityProposalHcrLink> {
    Ok(CapabilityProposalHcrLink {
        proposal_id: row.get(0)?,
        hcr_id: row.get(1)?,
        claim_id: row.get(2)?,
        run_id: row.get(3)?,
        operation: row.get(4)?,
        candidate_id: row.get(5)?,
        candidate_digest: row.get(6)?,
        artifact_ref: row.get(7)?,
        artifact_digest: row.get(8)?,
        evidence_digest: row.get(9)?,
        source_registry_snapshot_id: row.get(10)?,
        settlement_id: row.get(11)?,
        created_at: row.get(12)?,
    })
}
