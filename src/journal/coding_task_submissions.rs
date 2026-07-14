//! Durable exactly-once ownership for the fixed PR3A Harness submit.

use crate::domain::{InvocationId, RunId, SessionId};
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::Value;

pub(crate) enum CodingTaskSubmissionClaim {
    Claimed {
        invocation_id: InvocationId,
    },
    InProgress,
    Succeeded {
        invocation_id: InvocationId,
        result: Value,
    },
}

impl super::JournalStore {
    pub(crate) fn claim_coding_task_submission(
        &self,
        source_message_id: &str,
        request_digest: &str,
        proposed_invocation_id: &InvocationId,
        run_id: &RunId,
        session_id: &SessionId,
    ) -> Result<CodingTaskSubmissionClaim> {
        if source_message_id.trim().is_empty() {
            bail!("MISSING_SOURCE_MESSAGE_ID");
        }
        crate::capabilities::store::Sha256Digest::parse(request_digest)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, String, Option<String>)> = tx
            .query_row(
                "SELECT request_digest,invocation_id,status,result_json
                 FROM coding_task_submissions WHERE source_message_id=?1",
                params![source_message_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((persisted_digest, invocation_id, status, result_json)) = existing {
            tx.commit()?;
            if persisted_digest != request_digest {
                bail!("CODING_SUBMISSION_IDEMPOTENCY_CONFLICT");
            }
            return match status.as_str() {
                "running" => Ok(CodingTaskSubmissionClaim::InProgress),
                "succeeded" => {
                    let result = result_json
                        .ok_or_else(|| anyhow::anyhow!("CODING_SUBMISSION_RESULT_MISSING"))?;
                    Ok(CodingTaskSubmissionClaim::Succeeded {
                        invocation_id: InvocationId(invocation_id),
                        result: serde_json::from_str(&result)?,
                    })
                }
                "failed" => bail!("CODING_SUBMISSION_PREVIOUSLY_FAILED"),
                _ => bail!("CODING_SUBMISSION_INVALID_STATUS"),
            };
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO coding_task_submissions
             (source_message_id,request_digest,invocation_id,origin_run_id,origin_session_id,
              status,result_json,error_code,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,'running',NULL,NULL,?6,?6)",
            params![
                source_message_id,
                request_digest,
                proposed_invocation_id.0,
                run_id.0,
                session_id.0,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(CodingTaskSubmissionClaim::Claimed {
            invocation_id: proposed_invocation_id.clone(),
        })
    }

    pub(crate) fn complete_coding_task_submission(
        &self,
        source_message_id: &str,
        invocation_id: &InvocationId,
        result: &Value,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let changed = conn.execute(
            "UPDATE coding_task_submissions
             SET status='succeeded',result_json=?3,error_code=NULL,updated_at=?4
             WHERE source_message_id=?1 AND invocation_id=?2 AND status='running'",
            params![
                source_message_id,
                invocation_id.0,
                serde_json::to_string(result)?,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        if changed != 1 {
            bail!("CODING_SUBMISSION_COMPLETE_CONFLICT");
        }
        Ok(())
    }

    pub(crate) fn fail_coding_task_submission(
        &self,
        source_message_id: &str,
        invocation_id: &InvocationId,
        error_code: &str,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        conn.execute(
            "UPDATE coding_task_submissions
             SET status='failed',error_code=?3,updated_at=?4
             WHERE source_message_id=?1 AND invocation_id=?2 AND status='running'",
            params![
                source_message_id,
                invocation_id.0,
                error_code,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}
