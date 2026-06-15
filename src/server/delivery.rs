use crate::adapters::{HttpConnectorAdapter, InvocationAdapter};
use crate::config::KernelConfig;
use crate::domain::{EventId, JournalEventKind, ValidatedEvent};
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::OpenAiCompatibleLlm;
use crate::runtime::{outbox_dispatcher::dispatch_once, Runtime};
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

pub(crate) fn start_worker_loop(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    gateway: Arc<Gateway>,
    running: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while running.load(Ordering::SeqCst) {
            match process_next_worker_job(&config, &journal, &gateway) {
                Ok(true) => {}
                Ok(false) => thread::sleep(Duration::from_millis(100)),
                Err(error) => {
                    eprintln!(
                        "kernel worker loop failed category={}",
                        error_category(&error)
                    );
                    thread::sleep(Duration::from_millis(250));
                }
            }
        }
    })
}

pub(crate) fn recover_undelivered_ingress(journal: Arc<JournalStore>) -> Result<usize> {
    let events = journal.undelivered_ingress_events()?;
    let mut recovered = 0;
    for event in events {
        if let Some(event_id) = event.payload.get("event_id").and_then(Value::as_str) {
            recovered += 1;
            journal.enqueue_worker_job(&EventId(event_id.to_string()))?;
        }
    }
    Ok(recovered)
}

fn process_next_worker_job(
    config: &KernelConfig,
    journal: &JournalStore,
    gateway: &Gateway,
) -> Result<bool> {
    let Some(source_event_id) = journal.lease_next_worker_job()? else {
        return Ok(false);
    };
    let event_id = source_event_id.0.clone();
    let result = deliver_worker_event(config.clone(), journal, gateway, &source_event_id);
    if let Err(error) = result {
        let category = error_category(&error);
        eprintln!(
            "kernel worker delivery failed event={} category={}",
            short_id(&event_id),
            category
        );
        let _ = journal.fail_worker_job(&source_event_id, &category);
        let _ = journal.append_event(
            JournalEventKind::RunCompleted,
            None,
            None,
            Some(&event_id),
            json!({
                "status": "Failed",
                "reason": "worker_delivery_failed",
                "error_category": category,
            }),
        );
    } else if let Err(error) = journal.succeed_worker_job(&source_event_id) {
        eprintln!(
            "kernel worker job success update failed event={} category={}",
            short_id(&event_id),
            error_category(&error)
        );
    }
    Ok(true)
}

fn deliver_worker_event(
    config: KernelConfig,
    journal: &JournalStore,
    gateway: &Gateway,
    source_event_id: &EventId,
) -> Result<()> {
    let event = journal
        .ingress_event_by_event_id(&source_event_id.0)?
        .ok_or_else(|| anyhow::anyhow!("missing_ingress_event"))?;
    let validated = gateway.recover_validated_event(&event)?;
    deliver_event(config, journal, gateway, validated)
}

fn deliver_event(
    config: KernelConfig,
    journal: &JournalStore,
    gateway: &Gateway,
    validated: ValidatedEvent,
) -> Result<()> {
    let adapter = HttpConnectorAdapter::new(
        config.connector_execute_url.clone(),
        config.ipc_token.clone(),
    );
    let llm = OpenAiCompatibleLlm::new(
        config.openai_base_url.clone(),
        config.openai_api_key.clone(),
        config.model.clone(),
        config.model_timeout_ms,
    )
    .with_fallback(
        config.fallback_openai_base_url.clone(),
        config.fallback_openai_api_key.clone(),
        config.fallback_model.clone(),
    );
    let llm = Box::new(llm);
    let runtime = Runtime::new(config.clone(), llm, adapter);
    runtime.deliver(journal, gateway, validated)?;
    Ok(())
}

fn error_category(error: &anyhow::Error) -> String {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("timeout") {
        "timeout".to_string()
    } else if message.contains("connector execute failed") {
        "connector_execute_failed".to_string()
    } else if message.contains("target_session") {
        "target_session_mismatch".to_string()
    } else {
        "runtime_failed".to_string()
    }
}

fn short_id(value: &str) -> String {
    if value.len() <= 10 {
        value.to_string()
    } else {
        format!("{}...{}", &value[..4], &value[value.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use chrono::Utc;
    use serde_json::json;
    struct OkAdapter;
    impl InvocationAdapter for OkAdapter {
        fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
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
    fn run_dispatcher_sends_pending_outbox() -> Result<()> {
        let journal = Arc::new(JournalStore::in_memory()?);
        let running = Arc::new(AtomicBool::new(true));
        let running_stop = Arc::clone(&running);

        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "stdout.send_text".into(),
                arguments: json!({"text": "hello"}),
                idempotency_key: Some("idem_dispatch".into()),
            },
            "decision_dispatch".into(),
        );
        let invocation_id = approved.intent().invocation_id.clone();
        journal.queue_outbox_dispatch(&approved, None)?;

        let journal_ref = Arc::clone(&journal);
        let handle = thread::spawn(move || {
            run_dispatcher(&journal_ref, &OkAdapter, running, 10);
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = handle.join();

        assert_eq!(
            journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
            Some(&OutboxDispatchStatus::Succeeded)
        );
        assert!(journal.events()?.iter().any(|e| e.kind == JournalEventKind::ReceiptReceived));
        assert!(journal.events()?.iter().any(|e| e.kind == JournalEventKind::DispatchStarted));
        assert!(journal.verify_hash_chain()?);
        Ok(())
    }

    #[test]
    fn run_dispatcher_skips_unknown_outbox() -> Result<()> {
        let journal = Arc::new(JournalStore::in_memory()?);
        let running = Arc::new(AtomicBool::new(true));
        let running_stop = Arc::clone(&running);

        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "stdout.send_text".into(),
                arguments: json!({"text": "hello"}),
                idempotency_key: Some("idem_unknown".into()),
            },
            "decision_unknown".into(),
        );
        let invocation_id = approved.intent().invocation_id.clone();
        journal.queue_outbox_dispatch(&approved, None)?;
        {
            let conn = journal.conn.lock().unwrap();
            conn.execute(
                "UPDATE outbox_dispatches SET status = ?1 WHERE invocation_id = ?2",
                rusqlite::params![OutboxDispatchStatus::Unknown.as_str(), invocation_id.0],
            )?;
        }

        let journal_ref = Arc::clone(&journal);
        let handle = thread::spawn(move || {
            run_dispatcher(&journal_ref, &OkAdapter, running, 10);
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = handle.join();

        assert_eq!(
            journal.outbox_dispatch_status(&invocation_id)?.as_ref(),
            Some(&OutboxDispatchStatus::Unknown)
        );
        Ok(())
    }

    #[test]
    fn run_dispatcher_stops_on_shutdown() -> Result<()> {
        let journal = Arc::new(JournalStore::in_memory()?);
        let running = Arc::new(AtomicBool::new(true));
        let running_stop = Arc::clone(&running);

        let journal_ref = Arc::clone(&journal);
        let handle = thread::spawn(move || {
            run_dispatcher(&journal_ref, &OkAdapter, running, 10);
        });

        running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = handle.join();
        Ok(())
    }

    #[test]
    fn disabled_dispatcher_loop_returns_without_draining_outbox() -> Result<()> {
        use crate::config::KernelConfig;
        use std::path::PathBuf;

        let config = KernelConfig {
            db_path: PathBuf::from(":memory:"),
            data_dir: PathBuf::from(".agent-core-test"),
            agent_id: crate::domain::AgentId("main".to_string()),
            root_dir: PathBuf::from("."),
            kernel_port: 4130,
            connector_execute_url: "http://127.0.0.1:4131/v1/execute".to_string(),
            ipc_token: "test-token".to_string(),
            feishu_allowed_open_ids: vec![],
            feishu_allowed_chat_ids: vec![],
            feishu_require_group_mention: true,
            openai_base_url: String::new(),
            openai_api_key: String::new(),
            model: String::new(),
            fallback_openai_base_url: String::new(),
            fallback_openai_api_key: String::new(),
            fallback_model: String::new(),
            model_timeout_ms: 100,
            context_recent_messages: 6,
            context_max_block_chars: 4_000,
            outbox_dispatcher_enabled: false,
            outbox_dispatcher_poll_interval_ms: 10,
        };

        let journal = Arc::new(JournalStore::in_memory()?);
        let approved = ApprovedInvocation::new(
            InvocationIntent {
                invocation_id: InvocationId::new(),
                run_id: RunId::new(),
                operation: "stdout.send_text".into(),
                arguments: json!({"text": "hello"}),
                idempotency_key: Some("idem_disabled".into()),
            },
            "decision_disabled".into(),
        );
        let invocation_id = approved.intent().invocation_id.clone();
        journal.queue_outbox_dispatch(&approved, None)?;

        let handle = start_outbox_dispatcher_loop(
            config,
            Arc::clone(&journal),
            Arc::new(AtomicBool::new(true)),
        );
        let finished = handle
            .join()
            .map_err(|_| anyhow::anyhow!("dispatcher thread panicked"))?;
        assert_eq!(finished, ());

        assert_eq!(
            journal
                .outbox_dispatch_status(&invocation_id)?
                .as_ref(),
            Some(&OutboxDispatchStatus::Pending),
            "disabled dispatcher must not consume pending outbox"
        );
        Ok(())
    }

    #[test]
    fn run_dispatcher_drains_multiple_pending_rows() -> Result<()> {
        use std::sync::Mutex;

        struct CountingAdapter(Arc<Mutex<Vec<InvocationId>>>);
        impl InvocationAdapter for CountingAdapter {
            fn execute(&self, invocation: &ApprovedInvocation) -> Result<Receipt> {
                self.0
                    .lock()
                    .unwrap()
                    .push(invocation.intent().invocation_id.clone());
                Ok(Receipt {
                    invocation_id: invocation.intent().invocation_id.clone(),
                    status: ReceiptStatus::Succeeded,
                    external_ref: Some("test".into()),
                    output: json!({"text": "ok"}),
                    occurred_at: Utc::now(),
                })
            }
        }

        let journal = Arc::new(JournalStore::in_memory()?);
        let running = Arc::new(AtomicBool::new(true));
        let running_stop = Arc::clone(&running);

        let calls = Arc::new(Mutex::new(vec![]));
        let mut invocation_ids = vec![];
        for idx in 0..3 {
            let approved = ApprovedInvocation::new(
                InvocationIntent {
                    invocation_id: InvocationId(format!("reply:multi_{idx}")),
                    run_id: RunId(format!("run_multi_{idx}")),
                    operation: "stdout.send_text".into(),
                    arguments: json!({"text": "hello"}),
                    idempotency_key: Some(format!("idem_multi_{idx}")),
                },
                format!("decision_multi_{idx}"),
            );
            invocation_ids.push(approved.intent().invocation_id.clone());
            journal.queue_outbox_dispatch(&approved, None)?;
        }

        let journal_ref = Arc::clone(&journal);
        let calls_ref = Arc::clone(&calls);
        let handle = thread::spawn(move || {
            run_dispatcher(
                &journal_ref,
                &CountingAdapter(calls_ref),
                running,
                5,
            );
        });

        // Drive a bounded number of polls: 3 dispatches + at least one extra
        // tick so the loop sees an empty queue. 200ms @ 5ms poll >= 40 ticks.
        std::thread::sleep(std::time::Duration::from_millis(200));
        running_stop.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = handle.join();

        let pushed = calls.lock().unwrap().clone();
        assert_eq!(
            pushed.len(),
            invocation_ids.len(),
            "every pending row must be dispatched exactly once"
        );
        for invocation_id in &invocation_ids {
            assert!(
                pushed.iter().any(|id| id == invocation_id),
                "invocation {invocation_id:?} must have been dispatched"
            );
            assert_eq!(
                journal
                    .outbox_dispatch_status(invocation_id)?
                    .as_ref(),
                Some(&OutboxDispatchStatus::Succeeded),
                "invocation {invocation_id:?} must be succeeded"
            );
            let dispatch_started = journal
                .events()?
                .iter()
                .filter(|event| {
                    event.kind == JournalEventKind::DispatchStarted
                        && event.correlation_id.as_deref()
                            == Some(invocation_id.0.as_str())
                })
                .count();
            assert_eq!(dispatch_started, 1);
            let receipt = journal
                .events()?
                .iter()
                .filter(|event| {
                    event.kind == JournalEventKind::ReceiptReceived
                        && event.correlation_id.as_deref()
                            == Some(invocation_id.0.as_str())
                })
                .count();
            assert_eq!(receipt, 1);
        }
        assert!(journal.verify_hash_chain()?);
        Ok(())
    }
}

pub(crate) fn start_outbox_dispatcher_loop(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    running: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if !config.outbox_dispatcher_enabled {
            return;
        }
        let adapter = HttpConnectorAdapter::new(
            config.connector_execute_url.clone(),
            config.ipc_token.clone(),
        );
        run_dispatcher(&journal, &adapter, running, config.outbox_dispatcher_poll_interval_ms)
    })
}

fn run_dispatcher(
    journal: &JournalStore,
    adapter: &impl InvocationAdapter,
    running: Arc<AtomicBool>,
    poll_interval_ms: u64,
) {
    let poll = Duration::from_millis(poll_interval_ms);
    while running.load(Ordering::SeqCst) {
        match dispatch_once(journal, adapter) {
            Ok(true) => {}
            Ok(false) => thread::sleep(poll),
            Err(error) => {
                eprintln!(
                    "kernel outbox dispatcher failed category={}",
                    error_category(&error)
                );
                thread::sleep(poll);
            }
        }
    }
}
