use crate::adapters::InvocationAdapter;
use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::Result;

/// Lease exactly one pending or retryable_failed outbox dispatch, execute it
/// through the adapter, and handle the outcome.
///
/// Returns `true` if a dispatch was processed, `false` if no leasable row.
pub fn dispatch_once(journal: &JournalStore, adapter: &impl InvocationAdapter) -> Result<bool> {
    let Some(leased) = journal.lease_next_outbox_dispatch()? else {
        return Ok(false);
    };
    let run_id = leased.run_id.clone();
    let session_id = leased.session_id.clone();
    let invocation_id = leased.invocation_id.clone();

    let intent = InvocationIntent {
        invocation_id: leased.invocation_id,
        run_id: leased.run_id,
        operation: leased.operation,
        arguments: leased.arguments,
        idempotency_key: Some(leased.idempotency_key),
    };
    let approved = ApprovedInvocation::new(intent, leased.decision_id);

    match adapter.execute(&approved) {
        Ok(receipt) if receipt.status == ReceiptStatus::Succeeded => {
            journal.succeed_outbox_dispatch(&receipt, &run_id, session_id.as_ref())?;
        }
        Ok(receipt) if receipt.status == ReceiptStatus::Failed => {
            journal.fail_outbox_dispatch(
                &invocation_id,
                &run_id,
                session_id.as_ref(),
                "definite_failure",
            )?;
        }
        Ok(receipt) => {
            journal.unknown_outbox_dispatch(
                &invocation_id,
                &run_id,
                session_id.as_ref(),
                &format!("unknown_receipt_status:{:?}", receipt.status),
            )?;
        }
        Err(error) => {
            let category = DispatchErrorCategory::from_error(&error);
            journal.unknown_outbox_dispatch(
                &invocation_id,
                &run_id,
                session_id.as_ref(),
                category.as_str(),
            )?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rusqlite::params;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct AdapterCall {
        invocation_id: InvocationId,
        operation: String,
        idempotency_key: Option<String>,
        decision_id: String,
    }

    struct FakeAdapter(Arc<Mutex<Vec<AdapterCall>>>);

    impl InvocationAdapter for FakeAdapter {
        fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
            self.0.lock().unwrap().push(AdapterCall {
                invocation_id: invocation.intent().invocation_id.clone(),
                operation: invocation.intent().operation.clone(),
                idempotency_key: invocation.intent().idempotency_key.clone(),
                decision_id: invocation.decision_id.clone(),
            });
            Ok(Receipt {
                invocation_id: invocation.intent().invocation_id.clone(),
                status: ReceiptStatus::Succeeded,
                external_ref: Some("test".into()),
                output: json!({"text": "ok"}),
                occurred_at: Utc::now(),
            })
        }
    }

    struct DefiniteFailAdapter;

    impl InvocationAdapter for DefiniteFailAdapter {
        fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
            Ok(Receipt {
                invocation_id: invocation.intent().invocation_id.clone(),
                status: ReceiptStatus::Failed,
                external_ref: None,
                output: json!({"error": "bad_request"}),
                occurred_at: Utc::now(),
            })
        }
    }

    struct TimeoutAdapter;

    impl InvocationAdapter for TimeoutAdapter {
        fn execute(&self, _invocation: &ApprovedInvocation) -> Result<Receipt> {
            Err(anyhow::anyhow!("connection timeout"))
        }
    }

    #[test]
    fn no_pending_row_returns_false_and_does_not_call_adapter() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let calls = Arc::new(Mutex::new(vec![]));
        let adapter = FakeAdapter(calls.clone());

        let result = dispatch_once(&journal, &adapter)?;
        assert!(!result);
        assert!(calls.lock().unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn pending_row_calls_adapter_and_marks_succeeded() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let calls = Arc::new(Mutex::new(vec![]));
        let adapter = FakeAdapter(calls.clone());

        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "test.op".into(),
                arguments: json!({"key": "value"}),
                idempotency_key: Some("idem_test".into()),
            },
            "decision_test".into(),
        );
        journal.queue_outbox_dispatch(&approved, None)?;

        let result = dispatch_once(&journal, &adapter)?;
        assert!(result);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[AdapterCall {
                invocation_id: approved.intent().invocation_id.clone(),
                operation: "test.op".into(),
                idempotency_key: Some("idem_test".into()),
                decision_id: "decision_test".into(),
            }]
        );

        assert_eq!(
            journal
                .outbox_dispatch_status(&approved.intent().invocation_id)?
                .as_ref(),
            Some(&OutboxDispatchStatus::Succeeded)
        );

        let events = journal.events()?;
        let receipt_events: Vec<_> = events
            .iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .collect();
        assert_eq!(receipt_events.len(), 1);
        assert_eq!(
            receipt_events[0].correlation_id.as_deref(),
            Some(approved.intent().invocation_id.0.as_str())
        );

        assert!(journal.verify_hash_chain()?);
        Ok(())
    }

    #[test]
    fn dispatch_definite_failure_marks_failed_and_writes_receipt() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let adapter = DefiniteFailAdapter;

        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "test.op".into(),
                arguments: json!({"key": "value"}),
                idempotency_key: Some("idem_fail".into()),
            },
            "decision_fail".into(),
        );
        let invocation_id = approved.intent().invocation_id.clone();
        journal.queue_outbox_dispatch(&approved, None)?;

        let result = dispatch_once(&journal, &adapter)?;
        assert!(result);

        assert_eq!(
            journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
            Some(&OutboxDispatchStatus::Failed)
        );

        let events = journal.events()?;
        let receipt_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.kind == JournalEventKind::ReceiptReceived
                    && e.correlation_id.as_deref() == Some(invocation_id.0.as_str())
            })
            .collect();
        assert_eq!(receipt_events.len(), 1);

        assert!(journal.verify_hash_chain()?);
        Ok(())
    }

    #[test]
    fn dispatch_timeout_after_dispatch_started_marks_unknown() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let adapter = TimeoutAdapter;

        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "test.op".into(),
                arguments: json!({"key": "value"}),
                idempotency_key: Some("idem_timeout".into()),
            },
            "decision_timeout".into(),
        );
        let invocation_id = approved.intent().invocation_id.clone();
        journal.queue_outbox_dispatch(&approved, None)?;

        let result = dispatch_once(&journal, &adapter)?;
        assert!(result);

        assert_eq!(
            journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
            Some(&OutboxDispatchStatus::Unknown)
        );

        let events = journal.events()?;
        let unknown_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.kind == JournalEventKind::OutboxDispatchUnknown
                    && e.correlation_id.as_deref() == Some(invocation_id.0.as_str())
            })
            .collect();
        assert_eq!(unknown_events.len(), 1);

        assert!(journal.verify_hash_chain()?);
        Ok(())
    }

    #[test]
    fn leased_outbox_is_dispatching_before_adapter_call() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let read_journal = &journal;

        struct InspectAdapter<'a>(&'a JournalStore);
        impl InvocationAdapter for InspectAdapter<'_> {
            fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
                let status = self
                    .0
                    .outbox_dispatch_status(&invocation.intent().invocation_id)?;
                assert_eq!(status.as_ref(), Some(&OutboxDispatchStatus::Dispatching));
                Ok(Receipt {
                    invocation_id: invocation.intent().invocation_id.clone(),
                    status: ReceiptStatus::Succeeded,
                    external_ref: Some("test".into()),
                    output: json!({"text": "ok"}),
                    occurred_at: Utc::now(),
                })
            }
        }

        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "test.op".into(),
                arguments: json!({"key": "value"}),
                idempotency_key: Some("idem_inspect".into()),
            },
            "decision_inspect".into(),
        );
        let invocation_id = approved.intent().invocation_id.clone();
        journal.queue_outbox_dispatch(&approved, None)?;

        let adapter = InspectAdapter(read_journal);
        dispatch_once(&journal, &adapter)?;

        assert_eq!(
            journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
            Some(&OutboxDispatchStatus::Succeeded)
        );
        assert!(journal.verify_hash_chain()?);
        Ok(())
    }

    #[test]
    fn unknown_dispatch_is_not_leased() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "test.op".into(),
                arguments: json!({"key": "value"}),
                idempotency_key: Some("idem_unknown_lease".into()),
            },
            "decision_unknown_lease".into(),
        );
        journal.queue_outbox_dispatch(&approved, None)?;
        {
            let conn = journal.conn.lock().unwrap();
            conn.execute(
                "UPDATE outbox_dispatches SET status = ?1 WHERE invocation_id = ?2",
                params![
                    OutboxDispatchStatus::Unknown.as_str(),
                    approved.intent().invocation_id.0
                ],
            )?;
        }

        let adapter = FakeAdapter(Arc::new(Mutex::new(vec![])));
        let result = dispatch_once(&journal, &adapter)?;
        assert!(!result, "must not lease unknown dispatch");
        Ok(())
    }

    #[test]
    fn succeeded_dispatch_is_not_leased() -> Result<()> {
        let journal = JournalStore::in_memory()?;
        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "test.op".into(),
                arguments: json!({"key": "value"}),
                idempotency_key: Some("idem_success_lease".into()),
            },
            "decision_success_lease".into(),
        );
        journal.queue_outbox_dispatch(&approved, None)?;
        {
            let conn = journal.conn.lock().unwrap();
            conn.execute(
                "UPDATE outbox_dispatches SET status = ?1 WHERE invocation_id = ?2",
                params![
                    OutboxDispatchStatus::Succeeded.as_str(),
                    approved.intent().invocation_id.0
                ],
            )?;
        }

        let adapter = FakeAdapter(Arc::new(Mutex::new(vec![])));
        let result = dispatch_once(&journal, &adapter)?;
        assert!(!result, "must not lease succeeded dispatch");
        Ok(())
    }
}
