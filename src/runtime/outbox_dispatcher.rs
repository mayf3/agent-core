use crate::adapters::InvocationAdapter;
use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::Result;

/// Lease exactly one pending outbox dispatch, execute it through the adapter,
/// and mark it succeeded. Returns `true` if a dispatch was processed, `false`
/// if no pending row was available.
pub fn dispatch_once(journal: &JournalStore, adapter: &impl InvocationAdapter) -> Result<bool> {
    let Some(leased) = journal.lease_next_outbox_dispatch()? else {
        return Ok(false);
    };

    let intent = InvocationIntent {
        invocation_id: leased.invocation_id,
        run_id: leased.run_id.clone(),
        operation: leased.operation,
        arguments: leased.arguments,
        idempotency_key: Some(leased.idempotency_key),
    };
    let approved = ApprovedInvocation::new(intent, leased.decision_id);
    let receipt = adapter.execute(&approved)?;
    journal.succeed_outbox_dispatch(&receipt, &leased.run_id, leased.session_id.as_ref())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
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
                .as_deref(),
            Some("succeeded")
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
}
