//! Decision-time revalidation of Proposal -> external Receipt trust bindings.
//!
//! New proposals use the generic receipt binding. Historical HCR bindings stay
//! readable and decidable without creating new HCR facts.

use super::activation_core::Binding;
use super::trusted_capability_activation::TrustedDecisionIdentity;
use crate::capabilities::store::Sha256Digest;
use crate::domain::{
    compute_acceptance_binding_digest, compute_external_receipt_digest, AgentId, ExternalOutcome,
    RunPrincipal, SCHEMA_VERSION, TRUSTED_ACCEPTANCE_ISSUER,
};
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub(super) fn load_validated_binding(
    conn: &Connection,
    identity: &TrustedDecisionIdentity,
    expected_agent: &AgentId,
) -> Result<Binding> {
    let binding = match load_receipt_binding(conn, identity)? {
        Some(binding) => binding,
        None => load_legacy_binding(conn, identity)?
            .ok_or_else(|| anyhow::anyhow!("TRUSTED_APPROVAL_NOT_FOUND"))?,
    };
    validate_identity(&binding, identity, expected_agent)?;
    if binding.receipt_binding {
        validate_authoritative_receipt(conn, &binding)?;
    } else {
        validate_authoritative_hcr(conn, &binding)?;
    }
    Ok(binding)
}

fn load_receipt_binding(
    conn: &Connection,
    identity: &TrustedDecisionIdentity,
) -> Result<Option<Binding>> {
    conn.query_row(
        "SELECT p.status,p.submitter_principal_id,p.target_agent_id,p.origin_session_id,
                p.origin_run_id,p.artifact_digest,p.artifact_ref,p.evidence_digest,
                p.manifest_digest,p.expected_active_snapshot_id,p.requested_operations_json,p.expires_at,
                l.operation,l.candidate_digest,l.artifact_digest,l.source_registry_snapshot_id,
                l.origin_run_id,'' AS hcr_id,'' AS claim_id,l.candidate_id,l.artifact_ref,
                l.evidence_digest,'' AS settlement_id,a.owner_principal_id,
                a.source_registry_snapshot_id,a.candidate_digest,a.artifact_digest,
                a.manifest_digest,a.decision_nonce,a.status,a.decision_id,
                a.decision_payload_digest,a.decision_result_json,a.decided_by,
                a.activated_snapshot_id,a.host_deployment_id,a.activation_error,a.expires_at,
                p.proposal_id,l.request_digest,l.receipt_digest,l.issuer_principal_id,
                l.acceptance_outcome,l.contract_catalog_version,l.profile_id,
                l.profile_catalog_version,l.acceptance_invocation_id
         FROM capability_governance_approvals a
         JOIN capability_change_proposals p ON p.proposal_id=a.proposal_id
         JOIN capability_proposal_receipt_links l ON l.proposal_id=p.proposal_id
         WHERE a.approval_id=?1 AND a.proposal_id=?2",
        params![identity.approval_id, identity.proposal_id],
        |row| row_to_binding(row, true, true),
    )
    .optional()
    .map_err(Into::into)
}

fn load_legacy_binding(
    conn: &Connection,
    identity: &TrustedDecisionIdentity,
) -> Result<Option<Binding>> {
    let sql =
        "SELECT p.status,p.submitter_principal_id,p.target_agent_id,p.origin_session_id,
                p.origin_run_id,p.artifact_digest,p.artifact_ref,p.evidence_digest,
                p.manifest_digest,p.expected_active_snapshot_id,p.requested_operations_json,p.expires_at,
                l.operation,l.candidate_digest,l.artifact_digest,l.source_registry_snapshot_id,
                l.run_id,l.hcr_id,l.claim_id,l.candidate_id,l.artifact_ref,l.evidence_digest,
                l.settlement_id,a.owner_principal_id,a.source_registry_snapshot_id,
                a.candidate_digest,a.artifact_digest,a.manifest_digest,a.decision_nonce,a.status,
                a.decision_id,a.decision_payload_digest,a.decision_result_json,a.decided_by,
                a.activated_snapshot_id,a.host_deployment_id,a.activation_error,a.expires_at,
                p.proposal_id,'' AS request_digest,'' AS receipt_digest,'' AS issuer,
                '' AS outcome,'' AS contract_version,'' AS profile_id,'' AS profile_version,
                '' AS acceptance_invocation
         FROM capability_governance_approvals a
         JOIN capability_change_proposals p ON p.proposal_id=a.proposal_id
         JOIN capability_proposal_hcr_links l ON l.proposal_id=p.proposal_id
         WHERE a.approval_id=?1 AND a.proposal_id=?2";
    conn.query_row(
        &sql,
        params![identity.approval_id, identity.proposal_id],
        |row| row_to_binding(row, true, false),
    )
    .optional()
    .map_err(Into::into)
}

fn row_to_binding(
    row: &rusqlite::Row<'_>,
    governance_approval: bool,
    receipt_binding: bool,
) -> rusqlite::Result<Binding> {
    Ok(Binding {
        governance_approval,
        receipt_binding,
        proposal_status: row.get(0)?,
        submitter: row.get(1)?,
        target_agent: row.get(2)?,
        origin_session: row.get(3)?,
        origin_run: row.get(4)?,
        proposal_artifact: row.get(5)?,
        proposal_artifact_ref: row.get(6)?,
        proposal_evidence: row.get(7)?,
        proposal_manifest: row.get(8)?,
        proposal_snapshot: row.get(9)?,
        requested_operations: row.get(10)?,
        proposal_expires_at: row.get(11)?,
        link_operation: row.get(12)?,
        link_candidate: row.get(13)?,
        link_artifact: row.get(14)?,
        link_snapshot: row.get(15)?,
        link_run: row.get(16)?,
        link_hcr: row.get(17)?,
        link_claim: row.get(18)?,
        link_candidate_id: row.get(19)?,
        link_artifact_ref: row.get(20)?,
        link_evidence: row.get(21)?,
        link_settlement: row.get(22)?,
        owner: row.get(23)?,
        approval_snapshot: row.get(24)?,
        approval_candidate: row.get(25)?,
        approval_artifact: row.get(26)?,
        approval_manifest: row.get(27)?,
        nonce: row.get(28)?,
        approval_status: row.get(29)?,
        decision_id: row.get(30)?,
        payload_digest: row.get(31)?,
        result_json: row.get(32)?,
        decided_by: row.get(33)?,
        activated_snapshot: row.get(34)?,
        deployment_id: row.get(35)?,
        activation_error: row.get(36)?,
        approval_expires_at: row.get(37)?,
        proposal_id: row.get(38)?,
        link_request_digest: row.get(39)?,
        link_receipt_digest: row.get(40)?,
        link_issuer: row.get(41)?,
        link_outcome: row.get(42)?,
        link_contract_version: row.get(43)?,
        link_profile_id: row.get(44)?,
        link_profile_version: row.get(45)?,
        link_acceptance_invocation: row.get(46)?,
    })
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
        || b.proposal_id != i.proposal_id
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
        || b.proposal_expires_at != b.approval_expires_at
    {
        bail!("TRUSTED_APPROVAL_BINDING_MISMATCH");
    }
    let ops: Vec<String> = serde_json::from_str(&b.requested_operations)?;
    if ops != [b.link_operation.clone()] || !safe_target_name(&b.link_operation) {
        bail!("TRUSTED_APPROVAL_OPERATION_MISMATCH");
    }
    Ok(())
}

fn validate_authoritative_receipt(conn: &Connection, b: &Binding) -> Result<()> {
    for digest in [
        &b.link_request_digest,
        &b.link_receipt_digest,
        &b.link_candidate,
        &b.link_artifact,
        &b.link_evidence,
    ] {
        Sha256Digest::parse(digest)?;
    }
    let acceptance_binding = compute_acceptance_binding_digest(
        &b.link_request_digest,
        &b.link_candidate,
        &b.link_artifact,
        &b.proposal_manifest,
        ExternalOutcome::Passed,
        &b.link_contract_version,
        &b.link_profile_id,
        &b.link_profile_version,
    );
    let expected_receipt = compute_external_receipt_digest(
        SCHEMA_VERSION,
        &b.link_acceptance_invocation,
        &b.link_issuer,
        &b.link_artifact,
        ExternalOutcome::Passed,
        &b.link_evidence,
        Some(&acceptance_binding),
    );
    if b.link_outcome != "passed"
        || b.link_issuer != TRUSTED_ACCEPTANCE_ISSUER
        || b.link_receipt_digest != expected_receipt
        || b.link_acceptance_invocation.is_empty()
        || b.link_contract_version.is_empty()
        || b.link_profile_id.is_empty()
        || b.link_profile_version.is_empty()
        || b.link_run != b.origin_run
    {
        bail!("TRUSTED_ACCEPTANCE_RECEIPT_MISMATCH");
    }
    validate_origin(conn, b)
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
    validate_origin(conn, b)
}

fn validate_origin(conn: &Connection, b: &Binding) -> Result<()> {
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

fn safe_target_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}
