use super::queue::append_event_tx;
use super::sqlite::JournalStore;
use crate::domain::*;
use anyhow::{anyhow, Result};
use chrono::{Duration, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::json;

impl JournalStore {
    pub fn lease_next_outbox_dispatch(&self) -> Result<Option<LeasedOutboxDispatch>> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("journal mutex poisoned"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let row = tx
            .query_row(
                "SELECT invocation_id, run_id, session_id, operation, arguments_json, idempotency_key, decision_id
                 FROM outbox_dispatches
                 WHERE status = 'pending' AND available_at <= ?1
                 ORDER BY available_at, created_at
                 LIMIT 1",
                params![now_text.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            invocation_id,
            run_id,
            session_id,
            operation,
            arguments_json,
            idempotency_key,
            decision_id,
        )) = row
        else {
            tx.commit()?;
            return Ok(None);
        };
        let arguments: serde_json::Value = serde_json::from_str(&arguments_json)?;
        let locked_until = (now + Duration::minutes(5)).to_rfc3339();
        let changed = tx.execute(
            "UPDATE outbox_dispatches
             SET status = 'dispatching',
                 attempts = attempts + 1,
                 locked_by = 'kernel-outbox',
                 locked_until = ?1,
                 updated_at = ?2
             WHERE invocation_id = ?3 AND status = 'pending'",
            params![
                locked_until.as_str(),
                now_text.as_str(),
                invocation_id.as_str()
            ],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let run_id_obj = RunId(run_id);
        let session_id_obj = session_id.map(SessionId);
        append_event_tx(
            &tx,
            JournalEventKind::DispatchStarted,
            Some(&run_id_obj),
            session_id_obj.as_ref(),
            Some(&invocation_id),
            json!({
                "operation": operation.as_str(),
                "attempted_at": now_text,
                "locked_until": locked_until,
            }),
        )?;
        tx.commit()?;
        Ok(Some(LeasedOutboxDispatch {
            invocation_id: InvocationId(invocation_id),
            run_id: run_id_obj,
            session_id: session_id_obj,
            operation,
            arguments,
            idempotency_key,
            decision_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use rusqlite::params;

    fn make_approved(op: &str) -> ApprovedInvocation {
        ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: op.to_string(),
                arguments: json!({"test": true}),
                idempotency_key: Some(format!("idem_{}", op)),
            },
            "decision_lease_test".to_string(),
        )
    }

    #[test]
    fn lease_pending_outbox_dispatch() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let approved = make_approved("lease_test");
        let session_id = SessionId::new();
        journal.queue_outbox_dispatch(&approved, Some(&session_id))?;

        let leased = journal.lease_next_outbox_dispatch()?;
        assert!(leased.is_some());
        let leased = leased.unwrap();
        assert_eq!(leased.invocation_id, approved.intent().invocation_id);
        assert_eq!(leased.run_id, approved.intent().run_id);
        assert_eq!(leased.session_id, Some(session_id));
        assert_eq!(leased.operation, "lease_test");
        assert_eq!(leased.arguments, json!({"test": true}));
        assert_eq!(leased.idempotency_key, "idem_lease_test");
        assert_eq!(leased.decision_id, approved.decision_id);

        let events = journal.events()?;
        let started: Vec<_> = events
            .iter()
            .filter(|e| e.kind == JournalEventKind::DispatchStarted)
            .collect();
        assert_eq!(started.len(), 1);
        assert_eq!(
            started[0].correlation_id.as_deref(),
            Some(approved.intent().invocation_id.0.as_str())
        );

        assert_eq!(
            journal
                .outbox_dispatch_status(&approved.intent().invocation_id)?
                .as_deref(),
            Some("dispatching")
        );

        {
            let conn = journal.conn.lock().unwrap();
            let (locked_by, locked_until, attempts): (String, String, i64) = conn
                .query_row(
                    "SELECT locked_by, locked_until, attempts FROM outbox_dispatches WHERE invocation_id = ?1",
                    params![approved.intent().invocation_id.0],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            let locked_until =
                chrono::DateTime::parse_from_rfc3339(&locked_until)?.with_timezone(&Utc);
            assert_eq!(locked_by, "kernel-outbox");
            assert!(locked_until > Utc::now());
            assert_eq!(attempts, 1);
        }

        assert!(journal.verify_hash_chain()?);
        Ok(())
    }

    #[test]
    fn lease_skips_non_pending_rows() -> Result<()> {
        let journal = JournalStore::in_memory()?;

        // succeeded dispatch
        let a_succeeded = make_approved("succeeded");
        journal.queue_outbox_dispatch(&a_succeeded, None)?;
        journal.start_outbox_dispatch(&a_succeeded, None)?;
        journal.succeed_outbox_dispatch(
            &Receipt {
                invocation_id: a_succeeded.intent().invocation_id.clone(),
                status: ReceiptStatus::Succeeded,
                external_ref: None,
                output: json!({}),
                occurred_at: Utc::now(),
            },
            &a_succeeded.intent().run_id,
            None,
        )?;

        // unknown dispatch - queue then manually set status
        let a_unknown = make_approved("unknown");
        journal.queue_outbox_dispatch(&a_unknown, None)?;
        {
            let conn = journal.conn.lock().unwrap();
            conn.execute(
                "UPDATE outbox_dispatches SET status = 'unknown' WHERE invocation_id = ?1",
                params![a_unknown.intent().invocation_id.0],
            )?;
        }

        // dispatching dispatch - queue and lease
        let a_dispatching = make_approved("dispatching");
        journal.queue_outbox_dispatch(&a_dispatching, None)?;
        let leased = journal.lease_next_outbox_dispatch()?;
        assert!(leased.is_some());
        assert_eq!(
            leased.unwrap().invocation_id,
            a_dispatching.intent().invocation_id
        );

        // pending dispatch that should be leasable
        let a_pending = make_approved("pending");
        journal.queue_outbox_dispatch(&a_pending, None)?;

        let leased = journal.lease_next_outbox_dispatch()?;
        assert!(leased.is_some());
        assert_eq!(
            leased.unwrap().invocation_id,
            a_pending.intent().invocation_id
        );

        // no more pending rows remain
        assert!(journal.lease_next_outbox_dispatch()?.is_none());
        assert!(journal.verify_hash_chain()?);
        Ok(())
    }
}
