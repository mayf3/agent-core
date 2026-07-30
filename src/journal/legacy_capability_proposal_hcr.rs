//! Read-only compatibility access for historical HCR-derived proposals.
//!
//! New proposals are bound to generic external Acceptance Receipts. These
//! readers exist only so already-persisted HCR proposals remain observable and
//! decidable during the compatibility window.

use crate::domain::CapabilityProposalHcrLink;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

impl super::JournalStore {
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
