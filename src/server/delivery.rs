use crate::adapters::HttpConnectorAdapter;
use crate::config::KernelConfig;
use crate::domain::{EventId, JournalEventKind, ValidatedEvent};
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::OpenAiCompatibleLlm;
use crate::runtime::Runtime;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::thread;

pub(crate) type DeliveryThreads = Arc<Mutex<Vec<thread::JoinHandle<()>>>>;

pub(crate) fn spawn_delivery(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    gateway: Arc<Gateway>,
    event: ValidatedEvent,
    deliveries: DeliveryThreads,
) {
    let event_id = event.event_id.0.clone();
    let handle = thread::spawn(move || {
        let source_event_id = EventId(event_id.clone());
        if let Err(error) = journal.start_worker_job(&source_event_id) {
            eprintln!(
                "kernel worker job start failed event={} category={}",
                short_id(&event_id),
                error_category(&error)
            );
        }
        if let Err(error) = deliver_event(config, &journal, &gateway, event) {
            let category = error_category(&error);
            eprintln!(
                "kernel async delivery failed event={} category={}",
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
                    "reason": "async_delivery_failed",
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
    });
    match deliveries.lock() {
        Ok(mut handles) => handles.push(handle),
        Err(_) => {
            eprintln!("kernel delivery tracker unavailable; waiting for delivery inline");
            let _ = handle.join();
        }
    }
}

pub(crate) fn prune_finished_deliveries(deliveries: &DeliveryThreads) {
    let mut finished = Vec::new();
    if let Ok(mut handles) = deliveries.lock() {
        let mut pending = Vec::with_capacity(handles.len());
        for handle in handles.drain(..) {
            if handle.is_finished() {
                finished.push(handle);
            } else {
                pending.push(handle);
            }
        }
        *handles = pending;
    }
    for handle in finished {
        if handle.join().is_err() {
            eprintln!("kernel delivery thread panicked");
        }
    }
}

pub(crate) fn drain_delivery_threads(deliveries: &DeliveryThreads) {
    let handles = match deliveries.lock() {
        Ok(mut handles) => handles.drain(..).collect::<Vec<_>>(),
        Err(_) => {
            eprintln!("kernel delivery tracker unavailable during shutdown");
            return;
        }
    };
    if !handles.is_empty() {
        println!("agent-core draining {} delivery thread(s)", handles.len());
    }
    for handle in handles {
        if handle.join().is_err() {
            eprintln!("kernel delivery thread panicked");
        }
    }
}

pub(crate) fn recover_undelivered_ingress(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    gateway: Arc<Gateway>,
    deliveries: DeliveryThreads,
) -> Result<usize> {
    let events = journal.undelivered_ingress_events()?;
    let mut recovered = 0;
    for event in events {
        match gateway.recover_validated_event(&event) {
            Ok(validated) => {
                recovered += 1;
                spawn_delivery(
                    config.clone(),
                    Arc::clone(&journal),
                    Arc::clone(&gateway),
                    validated,
                    Arc::clone(&deliveries),
                );
            }
            Err(error) => {
                journal.append_event(
                    JournalEventKind::RunCompleted,
                    None,
                    None,
                    event.payload.get("event_id").and_then(Value::as_str),
                    json!({
                        "status": "Failed",
                        "reason": "undelivered_ingress_recovery_failed",
                        "error_category": error_category(&error),
                    }),
                )?;
            }
        }
    }
    Ok(recovered)
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
