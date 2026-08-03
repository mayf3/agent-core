//! Durable, ordered ownership for Coding Harness submission attempts.

use crate::domain::{InvocationId, RunId, SessionId};
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::Value;

pub(crate) enum CodingTaskSubmissionClaim {
    Claimed {
        attempt_id: String,
        invocation_id: InvocationId,
    },
    InProgress,
    Succeeded {
        invocation_id: InvocationId,
        result: Value,
    },
    DefinitivelyRejected {
        error_code: String,
    },
}

impl super::JournalStore {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn claim_coding_task_submission(
        &self,
        source_message_id: &str,
        submission_call_key: &str,
        request_digest: &str,
        proposed_attempt_id: &str,
        proposed_invocation_id: &InvocationId,
        run_id: &RunId,
        session_id: &SessionId,
    ) -> Result<CodingTaskSubmissionClaim> {
        if source_message_id.trim().is_empty() {
            bail!("MISSING_SOURCE_MESSAGE_ID");
        }
        if submission_call_key.trim().is_empty() || proposed_attempt_id.trim().is_empty() {
            bail!("MISSING_CODING_SUBMISSION_IDENTITY");
        }
        crate::capabilities::store::Sha256Digest::parse(request_digest)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // The trusted tool-call position identifies a replay of the same
        // attempt.  A replay never creates or executes another Harness call.
        let replay: Option<(
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = tx
            .query_row(
                "SELECT attempt_id,source_message_id,request_digest,invocation_id,
                        result_json,error_code
                 FROM coding_task_submissions WHERE submission_call_key=?1",
                params![submission_call_key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        if let Some((
            attempt_id,
            persisted_source,
            persisted_digest,
            invocation_id,
            result,
            error,
        )) = replay
        {
            let status: String = tx.query_row(
                "SELECT status FROM coding_task_submissions WHERE attempt_id=?1",
                params![attempt_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            if persisted_source != source_message_id || persisted_digest != request_digest {
                bail!("CODING_SUBMISSION_REPLAY_IDENTITY_CONFLICT");
            }
            return match status.as_str() {
                "running" => Ok(CodingTaskSubmissionClaim::InProgress),
                "succeeded" => {
                    let result = result
                        .ok_or_else(|| anyhow::anyhow!("CODING_SUBMISSION_RESULT_MISSING"))?;
                    Ok(CodingTaskSubmissionClaim::Succeeded {
                        invocation_id: InvocationId(invocation_id),
                        result: serde_json::from_str(&result)?,
                    })
                }
                "definitively_rejected" => Ok(CodingTaskSubmissionClaim::DefinitivelyRejected {
                    error_code: error.unwrap_or_else(|| "CODING_HARNESS_REJECTED".into()),
                }),
                "outcome_unknown" => bail!("CODING_SUBMISSION_OUTCOME_UNKNOWN"),
                _ => bail!("CODING_SUBMISSION_INVALID_STATUS"),
            };
        }

        // A distinct trusted tool call requests a new attempt.  Only a
        // definitive rejection opens the message slot for the next sequence.
        let previous: Option<(i64, String)> = tx
            .query_row(
                "SELECT attempt_sequence,status
                 FROM coding_task_submissions
                 WHERE source_message_id=?1
                 ORDER BY attempt_sequence DESC LIMIT 1",
                params![source_message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let attempt_sequence = match previous {
            None => 1,
            Some((_, status)) if status == "running" => {
                tx.commit()?;
                bail!("CODING_TASK_ALREADY_IN_PROGRESS");
            }
            Some((_, status)) if status == "succeeded" => {
                tx.commit()?;
                bail!("CODING_SUBMISSION_ALREADY_SUCCEEDED");
            }
            Some((_, status)) if status == "outcome_unknown" => {
                tx.commit()?;
                bail!("CODING_SUBMISSION_OUTCOME_UNKNOWN");
            }
            Some((sequence, status)) if status == "definitively_rejected" => sequence + 1,
            Some(_) => {
                tx.commit()?;
                bail!("CODING_SUBMISSION_INVALID_STATUS");
            }
        };

        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO coding_task_submissions
             (attempt_id,source_message_id,attempt_sequence,submission_call_key,
              request_digest,invocation_id,origin_run_id,origin_session_id,
              status,result_json,error_code,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'running',NULL,NULL,?9,?9)",
            params![
                proposed_attempt_id,
                source_message_id,
                attempt_sequence,
                submission_call_key,
                request_digest,
                proposed_invocation_id.0,
                run_id.0,
                session_id.0,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(CodingTaskSubmissionClaim::Claimed {
            attempt_id: proposed_attempt_id.to_string(),
            invocation_id: proposed_invocation_id.clone(),
        })
    }

    pub(crate) fn complete_coding_task_submission(
        &self,
        attempt_id: &str,
        invocation_id: &InvocationId,
        result: &Value,
    ) -> Result<()> {
        self.finish_coding_task_submission(
            attempt_id,
            invocation_id,
            "succeeded",
            Some(result),
            None,
        )
    }

    pub(crate) fn reject_coding_task_submission(
        &self,
        attempt_id: &str,
        invocation_id: &InvocationId,
        error_code: &str,
    ) -> Result<()> {
        self.finish_coding_task_submission(
            attempt_id,
            invocation_id,
            "definitively_rejected",
            None,
            Some(error_code),
        )
    }

    pub(crate) fn mark_coding_task_submission_outcome_unknown(
        &self,
        attempt_id: &str,
        invocation_id: &InvocationId,
    ) -> Result<()> {
        self.finish_coding_task_submission(
            attempt_id,
            invocation_id,
            "outcome_unknown",
            None,
            Some("OUTCOME_UNKNOWN"),
        )
    }

    fn finish_coding_task_submission(
        &self,
        attempt_id: &str,
        invocation_id: &InvocationId,
        status: &str,
        result: Option<&Value>,
        error_code: Option<&str>,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let result_json = result.map(serde_json::to_string).transpose()?;
        let changed = conn.execute(
            "UPDATE coding_task_submissions
             SET status=?3,result_json=?4,error_code=?5,updated_at=?6
             WHERE attempt_id=?1 AND invocation_id=?2 AND status='running'",
            params![
                attempt_id,
                invocation_id.0,
                status,
                result_json,
                error_code,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        if changed != 1 {
            bail!("CODING_SUBMISSION_FINISH_CONFLICT");
        }
        Ok(())
    }
}
