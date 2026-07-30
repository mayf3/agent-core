//! Generic acceptance-receipt-backed Capability Proposal persistence.

use crate::capabilities::store::Sha256Digest;
use crate::domain::capability_change::CapabilityChangeProposal;
use crate::domain::*;
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

impl super::JournalStore {
    /// Atomically persist a generic external acceptance Receipt binding, the
    /// existing Proposal, and its human Approval identity.
    pub fn create_proposal_with_receipt(
        &self,
        proposal: &CapabilityChangeProposal,
        link: &CapabilityProposalReceiptLink,
    ) -> Result<String> {
        validate_fields(proposal, link)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing) = tx
            .query_row(
                "SELECT proposal_id,receipt_digest,artifact_digest,manifest_digest
                 FROM capability_proposal_receipt_links
                 WHERE request_digest=?1 AND operation=?2",
                params![link.request_digest, link.operation],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        {
            if existing.1 != link.receipt_digest
                || existing.2 != link.artifact_digest
                || existing.3 != link.manifest_digest
            {
                bail!("ACCEPTANCE_RECEIPT_REPLAY_CONFLICT");
            }
            tx.commit()?;
            return Ok(existing.0);
        }

        let (origin_session, origin_snapshot, principal_json): (String, String, String) = tx
            .query_row(
                "SELECT session_id,registry_snapshot_id,principal_json FROM runs WHERE id=?1",
                params![proposal.origin_run_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow::anyhow!("PROPOSAL_ORIGIN_RUN_NOT_FOUND"))?;
        let principal: RunPrincipal = serde_json::from_str(&principal_json)?;
        if origin_session != proposal.origin_session_id.0
            || origin_snapshot != link.source_registry_snapshot_id
            || principal.principal_id.0 != proposal.submitter_principal_id
            || link.origin_run_id != proposal.origin_run_id.0
            || link.origin_session_id != proposal.origin_session_id.0
        {
            bail!("PROPOSAL_ORIGIN_RUN_MISMATCH");
        }
        let session: (String, String) = tx
            .query_row(
                "SELECT channel,conversation_key FROM sessions WHERE id=?1",
                params![proposal.origin_session_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| anyhow::anyhow!("PROPOSAL_ORIGIN_SESSION_NOT_FOUND"))?;
        if session != ("Feishu".into(), proposal.submitter_principal_id.clone()) {
            bail!("PROPOSAL_REQUIRES_OWNER_PRIVATE_FEISHU_SESSION");
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
            "INSERT INTO capability_proposal_receipt_links
             (proposal_id,request_id,request_digest,acceptance_invocation_id,
              issuer_principal_id,operation,candidate_id,candidate_digest,
              artifact_ref,artifact_digest,manifest_ref,manifest_digest,
              evidence_digest,receipt_digest,acceptance_outcome,
              contract_catalog_version,profile_id,profile_catalog_version,
              source_registry_snapshot_id,origin_run_id,origin_session_id,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                     ?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                link.proposal_id,
                link.request_id,
                link.request_digest,
                link.acceptance_invocation_id,
                link.issuer_principal_id,
                link.operation,
                link.candidate_id,
                link.candidate_digest,
                link.artifact_ref,
                link.artifact_digest,
                link.manifest_ref,
                link.manifest_digest,
                link.evidence_digest,
                link.receipt_digest,
                link.acceptance_outcome,
                link.contract_catalog_version,
                link.profile_id,
                link.profile_catalog_version,
                link.source_registry_snapshot_id,
                link.origin_run_id,
                link.origin_session_id,
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
            "INSERT INTO capability_governance_approvals
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
                link.manifest_digest,
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
                "request_digest": link.request_digest,
                "acceptance_receipt_digest": link.receipt_digest,
                "acceptance_issuer": link.issuer_principal_id,
                "artifact_digest": proposal.artifact_digest,
                "manifest_digest": proposal.manifest_digest,
                "requested_operations": proposal.requested_operations,
                "expected_snapshot_id": proposal.expected_active_snapshot_id,
                "approval_id": approval_id,
                "approval_expires_at": proposal.expires_at,
            }),
        )?;
        tx.commit()?;
        Ok(proposal.proposal_id.clone())
    }

    pub fn load_proposal_receipt_link(
        &self,
        proposal_id: &str,
    ) -> Result<Option<CapabilityProposalReceiptLink>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT proposal_id,request_id,request_digest,acceptance_invocation_id,
                    issuer_principal_id,operation,candidate_id,candidate_digest,
                    artifact_ref,artifact_digest,manifest_ref,manifest_digest,
                    evidence_digest,receipt_digest,acceptance_outcome,
                    contract_catalog_version,profile_id,profile_catalog_version,
                    source_registry_snapshot_id,origin_run_id,origin_session_id,created_at
             FROM capability_proposal_receipt_links WHERE proposal_id=?1",
            params![proposal_id],
            |row| {
                Ok(CapabilityProposalReceiptLink {
                    proposal_id: row.get(0)?,
                    request_id: row.get(1)?,
                    request_digest: row.get(2)?,
                    acceptance_invocation_id: row.get(3)?,
                    issuer_principal_id: row.get(4)?,
                    operation: row.get(5)?,
                    candidate_id: row.get(6)?,
                    candidate_digest: row.get(7)?,
                    artifact_ref: row.get(8)?,
                    artifact_digest: row.get(9)?,
                    manifest_ref: row.get(10)?,
                    manifest_digest: row.get(11)?,
                    evidence_digest: row.get(12)?,
                    receipt_digest: row.get(13)?,
                    acceptance_outcome: row.get(14)?,
                    contract_catalog_version: row.get(15)?,
                    profile_id: row.get(16)?,
                    profile_catalog_version: row.get(17)?,
                    source_registry_snapshot_id: row.get(18)?,
                    origin_run_id: row.get(19)?,
                    origin_session_id: row.get(20)?,
                    created_at: row.get(21)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }
}

fn validate_fields(
    proposal: &CapabilityChangeProposal,
    link: &CapabilityProposalReceiptLink,
) -> Result<()> {
    for digest in [
        &link.request_digest,
        &link.candidate_digest,
        &link.artifact_digest,
        &link.manifest_digest,
        &link.evidence_digest,
        &link.receipt_digest,
    ] {
        Sha256Digest::parse(digest)?;
    }
    let acceptance_binding = compute_acceptance_binding_digest(
        &link.request_digest,
        &link.candidate_digest,
        &link.artifact_digest,
        &link.manifest_digest,
        ExternalOutcome::Passed,
        &link.contract_catalog_version,
        &link.profile_id,
        &link.profile_catalog_version,
    );
    let expected_receipt = compute_external_receipt_digest(
        SCHEMA_VERSION,
        &link.acceptance_invocation_id,
        &link.issuer_principal_id,
        &link.artifact_digest,
        ExternalOutcome::Passed,
        &link.evidence_digest,
        Some(&acceptance_binding),
    );
    if proposal.proposal_id != link.proposal_id
        || proposal.artifact_ref != link.artifact_ref
        || proposal.artifact_digest != link.artifact_digest
        || proposal.manifest_ref != link.manifest_ref
        || proposal.manifest_digest != link.manifest_digest
        || proposal.evidence_digest != link.evidence_digest
        || proposal.expected_active_snapshot_id != link.source_registry_snapshot_id
        || proposal.requested_operations != [link.operation.clone()]
        || link.acceptance_outcome != "passed"
        || link.receipt_digest != expected_receipt
        || link.issuer_principal_id != TRUSTED_ACCEPTANCE_ISSUER
        || link.acceptance_invocation_id.is_empty()
        || link.issuer_principal_id.is_empty()
    {
        bail!("ACCEPTANCE_RECEIPT_PROPOSAL_BINDING_MISMATCH");
    }
    Ok(())
}

fn insert_proposal(tx: &Transaction<'_>, proposal: &CapabilityChangeProposal) -> Result<()> {
    tx.execute(
        "INSERT INTO capability_change_proposals
         (proposal_id,submitter_principal_id,target_agent_id,origin_session_id,origin_run_id,
          artifact_ref,artifact_digest,manifest_ref,manifest_digest,evidence_ref,evidence_digest,
          requested_operations_json,risk_summary,expected_active_snapshot_id,status,created_at,expires_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'PendingApproval',?15,?16)",
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
            proposal.created_at.to_rfc3339(),
            proposal.expires_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}
