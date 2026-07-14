//! Decision-time revalidation of the full Proposal -> HCR trust chain.

use super::activation_core::Binding;
use super::trusted_capability_activation::{TrustedDecisionIdentity, CALCULATOR};
use crate::capabilities::store::Sha256Digest;
use crate::domain::{AgentId, RunPrincipal};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub(super) fn load_validated_binding(
    conn: &Connection,
    identity: &TrustedDecisionIdentity,
    expected_agent: &AgentId,
) -> Result<Binding> {
    let binding = load_binding(conn, identity)?;
    validate_identity(&binding, identity, expected_agent)?;
    validate_authoritative_hcr(conn, &binding)?;
    Ok(binding)
}

fn load_binding(conn: &Connection, identity: &TrustedDecisionIdentity) -> Result<Binding> {
    conn.query_row(
        "SELECT p.status,p.submitter_principal_id,p.target_agent_id,p.origin_session_id,
                p.origin_run_id,p.artifact_digest,p.artifact_ref,p.evidence_digest,
                p.manifest_digest,p.expected_active_snapshot_id,p.requested_operations_json,p.expires_at,
                l.operation,l.candidate_digest,l.artifact_digest,l.source_registry_snapshot_id,
                l.run_id,l.hcr_id,l.claim_id,l.candidate_id,l.artifact_ref,l.evidence_digest,
                l.settlement_id,a.owner_principal_id,a.source_registry_snapshot_id,
                a.candidate_digest,a.artifact_digest,a.manifest_digest,a.decision_nonce,a.status,
                a.decision_id,a.decision_payload_digest,a.decision_result_json,a.decided_by,
                a.activated_snapshot_id,a.host_deployment_id,a.activation_error,a.expires_at
         FROM capability_change_approvals a
         JOIN capability_change_proposals p ON p.proposal_id=a.proposal_id
         JOIN capability_proposal_hcr_links l ON l.proposal_id=p.proposal_id
         WHERE a.approval_id=?1 AND a.proposal_id=?2",
        params![identity.approval_id, identity.proposal_id],
        |r| {
            Ok(Binding {
                proposal_status: r.get(0)?,
                submitter: r.get(1)?,
                target_agent: r.get(2)?,
                origin_session: r.get(3)?,
                origin_run: r.get(4)?,
                proposal_artifact: r.get(5)?,
                proposal_artifact_ref: r.get(6)?,
                proposal_evidence: r.get(7)?,
                proposal_manifest: r.get(8)?,
                proposal_snapshot: r.get(9)?,
                requested_operations: r.get(10)?,
                proposal_expires_at: r.get(11)?,
                link_operation: r.get(12)?,
                link_candidate: r.get(13)?,
                link_artifact: r.get(14)?,
                link_snapshot: r.get(15)?,
                link_run: r.get(16)?,
                link_hcr: r.get(17)?,
                link_claim: r.get(18)?,
                link_candidate_id: r.get(19)?,
                link_artifact_ref: r.get(20)?,
                link_evidence: r.get(21)?,
                link_settlement: r.get(22)?,
                owner: r.get(23)?,
                approval_snapshot: r.get(24)?,
                approval_candidate: r.get(25)?,
                approval_artifact: r.get(26)?,
                approval_manifest: r.get(27)?,
                nonce: r.get(28)?,
                approval_status: r.get(29)?,
                decision_id: r.get(30)?,
                payload_digest: r.get(31)?,
                result_json: r.get(32)?,
                decided_by: r.get(33)?,
                activated_snapshot: r.get(34)?,
                deployment_id: r.get(35)?,
                activation_error: r.get(36)?,
                approval_expires_at: r.get(37)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| anyhow::anyhow!("TRUSTED_APPROVAL_NOT_FOUND"))
}

fn validate_identity(b: &Binding, i: &TrustedDecisionIdentity, agent: &AgentId) -> Result<()> {
    for digest in [
        &i.candidate_digest,
        &i.artifact_digest,
        &i.manifest_digest,
        &i.payload_digest,
    ] {
        Sha256Digest::parse(digest)?;
    }
    if i.decision_id.trim().is_empty()
        || i.principal_id.trim().is_empty()
        || b.owner != i.principal_id
        || b.submitter != i.principal_id
        || b.target_agent != agent.0
        || b.nonce != i.decision_nonce
        || b.proposal_snapshot != i.expected_source_snapshot_id
        || b.link_snapshot != i.expected_source_snapshot_id
        || b.approval_snapshot != i.expected_source_snapshot_id
        || b.link_candidate != i.candidate_digest
        || b.approval_candidate != i.candidate_digest
        || b.proposal_artifact != i.artifact_digest
        || b.proposal_artifact_ref != i.artifact_digest
        || b.link_artifact != i.artifact_digest
        || b.link_artifact_ref != i.artifact_digest
        || b.approval_artifact != i.artifact_digest
        || b.proposal_manifest != i.manifest_digest
        || b.approval_manifest != i.manifest_digest
        || b.proposal_evidence != b.link_evidence
        || b.link_operation != CALCULATOR
        || b.proposal_expires_at != b.approval_expires_at
    {
        bail!("TRUSTED_APPROVAL_BINDING_MISMATCH");
    }
    let ops: Vec<String> = serde_json::from_str(&b.requested_operations)?;
    if ops != [CALCULATOR] {
        bail!("TRUSTED_APPROVAL_OPERATION_MISMATCH");
    }
    Ok(())
}

fn validate_authoritative_hcr(conn: &Connection, b: &Binding) -> Result<()> {
    let hcr: (String, String, String, String, String) = conn
        .query_row(
            "SELECT status,session_id,principal_id,channel,chat_type
             FROM harness_change_requests WHERE request_id=?1",
            params![b.link_hcr],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|_| anyhow::anyhow!("TRUSTED_HCR_NOT_FOUND"))?;
    if hcr
        != (
            "succeeded".into(),
            b.origin_session.clone(),
            b.owner.clone(),
            "Feishu".into(),
            "p2p".into(),
        )
    {
        bail!("TRUSTED_HCR_ORIGIN_MISMATCH");
    }

    let session: (String, String) = conn
        .query_row(
            "SELECT channel,conversation_key FROM sessions WHERE id=?1",
            params![b.origin_session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| anyhow::anyhow!("PROPOSAL_ORIGIN_SESSION_NOT_FOUND"))?;
    if session != ("Feishu".into(), b.owner.clone()) {
        bail!("PROPOSAL_REQUIRES_OWNER_PRIVATE_FEISHU_SESSION");
    }

    let settlement: (String, String, String, String) = conn
        .query_row(
            "SELECT claim_id,run_id,result,evidence_set_digest FROM hcr_settlements
             WHERE settlement_id=?1 AND hcr_id=?2",
            params![b.link_settlement, b.link_hcr],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|_| anyhow::anyhow!("TRUSTED_SETTLEMENT_NOT_FOUND"))?;
    if settlement.0 != b.link_claim
        || settlement.1 != b.link_run
        || settlement.2 != "succeeded"
        || Sha256Digest::parse(&settlement.3).is_err()
    {
        bail!("TRUSTED_SETTLEMENT_MISMATCH");
    }

    let attempts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM hcr_gate_attempts WHERE hcr_id=?1 AND claim_id=?2 AND run_id=?3",
        params![b.link_hcr, b.link_claim, b.link_run],
        |r| r.get(0),
    )?;
    let evidence: i64 = conn.query_row(
        "SELECT COUNT(*) FROM hcr_gate_evidence e JOIN hcr_gate_attempts a
         ON a.gate_attempt_id=e.gate_attempt_id
         WHERE a.hcr_id=?1 AND a.claim_id=?2 AND a.run_id=?3",
        params![b.link_hcr, b.link_claim, b.link_run],
        |r| r.get(0),
    )?;
    if attempts != 5 || evidence != 5 {
        bail!("TRUSTED_GATE_SET_INCOMPLETE");
    }

    let receipt: (i64, String, String, String, String, String, String, String) = conn
        .query_row(
            "SELECT COUNT(*),overall_outcome,candidate_id,candidate_digest,artifact_ref,
                    artifact_digest,evidence_digest,invocation_id FROM hcr_receipt_identities
             WHERE hcr_id=?1 AND claim_id=?2 AND run_id=?3",
            params![b.link_hcr, b.link_claim, b.link_run],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .map_err(|_| anyhow::anyhow!("TRUSTED_RECEIPT_NOT_FOUND"))?;
    if receipt.0 != 1
        || receipt.1 != "CandidatePassed"
        || receipt.2 != b.link_candidate_id
        || receipt.3 != b.link_candidate
        || receipt.4 != b.link_artifact_ref
        || receipt.5 != b.link_artifact
        || receipt.6 != b.link_evidence
        || receipt.7.is_empty()
    {
        bail!("TRUSTED_RECEIPT_MISMATCH");
    }

    let origin: (String, String, String) = conn
        .query_row(
            "SELECT session_id,registry_snapshot_id,principal_json FROM runs WHERE id=?1",
            params![b.origin_run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| anyhow::anyhow!("PROPOSAL_ORIGIN_RUN_NOT_FOUND"))?;
    let principal: RunPrincipal = serde_json::from_str(&origin.2)?;
    if origin.0 != b.origin_session
        || origin.1 != b.proposal_snapshot
        || principal.principal_id.0 != b.owner
    {
        bail!("PROPOSAL_ORIGIN_RUN_MISMATCH");
    }
    Ok(())
}
