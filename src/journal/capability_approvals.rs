//! Read-side helpers for Kernel-owned capability Approval identities.

use crate::domain::{ApprovalReplayIdentity, CapabilityApproval, CapabilityApprovalStatus};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};

impl super::JournalStore {
    pub fn load_capability_approval(
        &self,
        approval_id: &str,
    ) -> Result<Option<CapabilityApproval>> {
        self.load_capability_approval_where("approval_id", approval_id)
    }

    pub fn load_capability_approval_by_proposal(
        &self,
        proposal_id: &str,
    ) -> Result<Option<CapabilityApproval>> {
        self.load_capability_approval_where("proposal_id", proposal_id)
    }

    fn load_capability_approval_where(
        &self,
        column: &str,
        value: &str,
    ) -> Result<Option<CapabilityApproval>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let sql = format!(
            "SELECT approval_id,proposal_id,owner_principal_id,
                    source_registry_snapshot_id,candidate_digest,artifact_digest,
                    manifest_digest,decision_nonce,status,decision_id,
                    decision_payload_digest,decision_result_json,decided_at,decided_by,
                    activated_snapshot_id,host_deployment_id,activation_error,
                    created_at,expires_at
             FROM capability_change_approvals WHERE {column}=?1"
        );
        conn.query_row(&sql, params![value], row_to_approval)
            .optional()
            .map_err(Into::into)
    }

    pub fn load_approval_replay_identity(
        &self,
        approval_id: &str,
    ) -> Result<Option<ApprovalReplayIdentity>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        conn.query_row(
            "SELECT approval_id,proposal_id,decision_nonce,status,decision_id,
                    decision_payload_digest,decision_result_json,decided_at,decided_by,
                    activated_snapshot_id,host_deployment_id,activation_error
             FROM capability_change_approvals WHERE approval_id=?1",
            params![approval_id],
            row_to_replay_identity,
        )
        .optional()
        .map_err(Into::into)
    }
}

fn parse_status(value: &str) -> rusqlite::Result<CapabilityApprovalStatus> {
    match value {
        "Pending" => Ok(CapabilityApprovalStatus::Pending),
        "Approved" => Ok(CapabilityApprovalStatus::Approved),
        "Rejected" => Ok(CapabilityApprovalStatus::Rejected),
        "ActivationFailed" => Ok(CapabilityApprovalStatus::ActivationFailed),
        "Expired" => Ok(CapabilityApprovalStatus::Expired),
        other => Err(decode_error(format!("unknown approval status: {other}"))),
    }
}

fn parse_timestamp(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| decode_error(format!("invalid approval timestamp: {error}")))
}

fn parse_optional_timestamp(value: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value.map(parse_timestamp).transpose()
}

fn decode_error(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn row_to_approval(row: &Row<'_>) -> rusqlite::Result<CapabilityApproval> {
    Ok(CapabilityApproval {
        approval_id: row.get(0)?,
        proposal_id: row.get(1)?,
        owner_principal_id: row.get(2)?,
        source_registry_snapshot_id: row.get(3)?,
        candidate_digest: row.get(4)?,
        artifact_digest: row.get(5)?,
        manifest_digest: row.get(6)?,
        decision_nonce: row.get(7)?,
        status: parse_status(&row.get::<_, String>(8)?)?,
        decision_id: row.get(9)?,
        decision_payload_digest: row.get(10)?,
        decision_result_json: row.get(11)?,
        decided_at: parse_optional_timestamp(row.get(12)?)?,
        decided_by: row.get(13)?,
        activated_snapshot_id: row.get(14)?,
        host_deployment_id: row.get(15)?,
        activation_error: row.get(16)?,
        created_at: parse_timestamp(row.get(17)?)?,
        expires_at: parse_timestamp(row.get(18)?)?,
    })
}

fn row_to_replay_identity(row: &Row<'_>) -> rusqlite::Result<ApprovalReplayIdentity> {
    Ok(ApprovalReplayIdentity {
        approval_id: row.get(0)?,
        proposal_id: row.get(1)?,
        decision_nonce: row.get(2)?,
        status: parse_status(&row.get::<_, String>(3)?)?,
        decision_id: row.get(4)?,
        decision_payload_digest: row.get(5)?,
        decision_result_json: row.get(6)?,
        decided_at: parse_optional_timestamp(row.get(7)?)?,
        decided_by: row.get(8)?,
        activated_snapshot_id: row.get(9)?,
        host_deployment_id: row.get(10)?,
        activation_error: row.get(11)?,
    })
}
