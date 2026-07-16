//! Trusted profile-contract test for generated hook-consumer services.
//!
//! This source is embedded in the Coding Harness and compiled independently
//! from the candidate. It verifies the immutable runtime adapter while the
//! model-generated module remains request-specific.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn main() {
    let candidate = match std::env::args().nth(1) {
        Some(value) => value,
        None => std::process::exit(2),
    };
    let page = r#"{"schema_version":"event.observe.v0","next_cursor":3,"has_more":false,"events":[{"schema_version":"event.observe.v0","event_id":"completed-1","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-1","payload":{"schema_version":"model.invocation.completed.v0","profile":"default","provider":"test","model":"model-a<img src=x onerror=alert(1)>","latency_ms":20,"input_tokens":10,"cached_input_tokens":2,"output_tokens":5,"reasoning_tokens":1,"total_tokens":16,"provider_usage_extensions":{"future_counter":7}}},{"schema_version":"event.observe.v0","event_id":"failed-1","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-15T11:00:00Z","run_id":"run-2","payload":{"schema_version":"model.invocation.failed.v0","profile":"analysis","provider":"test","model":"model-b","latency_ms":30,"error_category":"dependency_unavailable"}},{"schema_version":"event.observe.v0","event_id":"future-1","event_kind":"future.observed.fact.v9","occurred_at":"2026-07-15T12:00:00Z","payload":{"unknown":{"nested":true}}}]}"#;
    let mut child = Command::new(candidate)
        .arg("--profile-contract-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|_| std::process::exit(3));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(page.as_bytes())
        .unwrap_or_else(|_| std::process::exit(4));
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap_or_else(|_| std::process::exit(5));
    let status = child.wait().unwrap_or_else(|_| std::process::exit(6));
    for expected in [
        "\"ok\":true",
        "\"schema_version\":\"hook-consumer-service-contract-v0\"",
        "\"events_applied\":3",
        "\"html_nonempty\":true",
        "\"html_safe\":true",
        "\"html_runtime_metadata\":true",
        "\"rendered\":",
    ] {
        if !stdout.contains(expected) {
            eprintln!("missing trusted contract evidence: {expected}");
            std::process::exit(7);
        }
    }
    if !status.success() {
        std::process::exit(8);
    }
}
