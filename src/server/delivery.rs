use crate::adapters::{HttpConnectorAdapter, InvocationAdapter};
use crate::config::KernelConfig;
use crate::domain::{EventId, JournalEventKind, ValidatedEvent};
use crate::gateway::Gateway;
use crate::hook::{HookClient, HttpHookClient};
use crate::journal::JournalStore;
use crate::llm::OpenAiCompatibleLlm;
use crate::runtime::{outbox_dispatcher::dispatch_once, Runtime};
use crate::server::dispatcher_metrics::{DispatcherMetrics, LoopGuard};
use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Build an `OpenAiCompatibleLlm` from a `KernelConfig`. This is the SINGLE
/// production wiring path shared by `deliver` and tests — so the
/// `primary_tool_name_indexed` / `fallback_tool_name_indexed` config flags are
/// applied identically everywhere. The mode is explicit config, never inferred
/// from URL/host/model substrings. Primary and fallback are independent.
pub(crate) fn build_llm_from_config(config: &KernelConfig) -> OpenAiCompatibleLlm {
    let mut llm = OpenAiCompatibleLlm::new(
        config.openai_base_url.clone(),
        config.openai_api_key.clone(),
        config.model.clone(),
        config.model_timeout_ms,
    );
    if config.primary_tool_name_indexed {
        llm = llm.with_indexed_primary();
    }
    if config.fallback_tool_name_indexed {
        if !config.fallback_openai_base_url.is_empty() {
            llm = llm.with_indexed_fallback(
                config.fallback_openai_base_url.clone(),
                config.fallback_openai_api_key.clone(),
                config.fallback_model.clone(),
            );
        }
    } else if !config.fallback_openai_base_url.is_empty() {
        llm = llm.with_fallback(
            config.fallback_openai_base_url.clone(),
            config.fallback_openai_api_key.clone(),
            config.fallback_model.clone(),
        );
    }
    llm
}
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
            JournalEventKind::RunFailed,
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
    let llm: Box<dyn crate::llm::LlmClient> = Box::new(build_llm_from_config(&config));
    let mut runtime = Runtime::new(config.clone(), llm);
    if config.context_prepare_hook.enabled {
        let hook_client: Box<dyn HookClient> = Box::new(HttpHookClient::new());
        runtime = runtime.with_hook(hook_client, config.context_prepare_hook.clone());
    }
    if super::calculator_router::matches(&validated) {
        super::calculator_delivery::deliver(config, journal, gateway, validated)?;
        return Ok(());
    }
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
#[path = "delivery_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "hook_wiring_tests.rs"]
mod hook_wiring_tests;

pub(crate) fn start_outbox_dispatcher_loop(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    running: Arc<AtomicBool>,
    metrics: Arc<DispatcherMetrics>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if !config.outbox_dispatcher_enabled {
            return;
        }
        let adapter = HttpConnectorAdapter::new(
            config.connector_execute_url.clone(),
            config.ipc_token.clone(),
        );
        run_dispatcher(
            &journal,
            &adapter,
            running,
            config.outbox_dispatcher_poll_interval_ms,
            &metrics,
        )
    })
}

/// Phase 2 M2d follow-up: periodic approval-expiry sweep. When
/// `require_write_approval && write_approval_ttl_secs > 0`, this loop re-runs
/// `JournalStore::expire_stale_approvals` on a fixed interval so a long-running
/// server expires stalled approvals without a restart (PR #80 only ran it at
/// startup). The transition is identical to the startup path
/// (`AwaitingApproval` -> `Failed` + `ApprovalExpired`); this changes only
/// *scheduling*, not protocol/state semantics, and is a no-op when the TTL is
/// 0 (disabled) — so opt-out is byte-identical.
pub(crate) fn start_approval_expiry_loop(
    config: KernelConfig,
    journal: Arc<JournalStore>,
    running: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if !(config.require_write_approval && config.write_approval_ttl_secs > 0) {
            return;
        }
        let ttl = config.write_approval_ttl_secs;
        // Sweep at most once per TTL, but no less frequently than every minute,
        // so a short TTL still triggers promptly. A longer TTL means fewer
        // wakeups, not longer-than-TTL stalls.
        let sweep = Duration::from_secs(ttl.clamp(60, 3600));
        while running.load(Ordering::SeqCst) {
            thread::sleep(sweep);
            if !running.load(Ordering::SeqCst) {
                break;
            }
            match journal.expire_stale_approvals(ttl) {
                Ok(0) => {}
                Ok(n) => println!("agent-core expired {n} stale approval(s)"),
                Err(error) => eprintln!("agent-core approval expiry sweep failed: {error}"),
            }
        }
    })
}

fn run_dispatcher(
    journal: &JournalStore,
    adapter: &impl InvocationAdapter,
    running: Arc<AtomicBool>,
    poll_interval_ms: u64,
    metrics: &DispatcherMetrics,
) {
    // LoopGuard marks the metrics running=true on construction and clears it
    // on drop, including if the loop panics. This is what feeds
    // `outbox_dispatcher_running` in /health.
    let _guard = LoopGuard::new(metrics);
    let poll = Duration::from_millis(poll_interval_ms);
    while running.load(Ordering::SeqCst) {
        match dispatch_once(journal, adapter) {
            Ok(true) => {
                metrics.record_tick(Utc::now().to_rfc3339());
            }
            Ok(false) => {
                metrics.record_tick(Utc::now().to_rfc3339());
                thread::sleep(poll);
            }
            Err(error) => {
                let category = error_category(&error);
                eprintln!("kernel outbox dispatcher failed category={category}");
                metrics.record_tick(Utc::now().to_rfc3339());
                metrics.record_error_category(category);
                thread::sleep(poll);
            }
        }
    }
}
