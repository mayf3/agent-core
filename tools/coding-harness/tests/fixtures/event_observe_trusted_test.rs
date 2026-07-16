//! Trusted test binary for the hook-consumer-service (token dashboard) candidate.
//!
//! Pipes an event.observe.v0 page of model.invocation.completed.v0 events
//! to the candidate binary on stdin, reads the token-usage projection from
//! stdout, and asserts the aggregates are correct.
//!
//! Exit code: 0 = all tests passed, 1 = any test failed.
//!
//! Usage: event_observe_trusted_test <candidate_binary_path>

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: {} <candidate_binary_path>", args[0]);
        std::process::exit(1);
    }
    let candidate_path = &args[1];

    // An event.observe.v0 page with two completed invocations.
    // Event 1: run_1, gpt-4o, default profile, 100+10+200+5 = 315 total.
    // Event 2: run_1, gpt-4o, default profile, 50+0+30+0 = 80 total.
    // Event 3: run_2, claude-3, analyst profile, 20+0+10+0 = 30 total.
    //
    // Expected aggregates:
    //   by_date  2026-07-15 : input=170 cached=10 output=240 reasoning=5 total=425
    //   by_run   run_1      : input=150 cached=10 output=230 reasoning=5 total=395
    //   by_run   run_2      : input=20  cached=0  output=10  reasoning=0 total=30
    //   by_model gpt-4o     : input=150 cached=10 output=230 reasoning=5 total=395
    //   by_model claude-3   : input=20  cached=0  output=10  reasoning=0 total=30
    //   by_profile default  : input=150 cached=10 output=230 reasoning=5 total=395
    //   by_profile analyst  : input=20  cached=0  output=10  reasoning=0 total=30
    let observe_page = r#"{"schema_version":"event-observe-v0","next_cursor":null,"has_more":false,"events":[
{"event_id":"evt_1","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run_1","principal_id":"principal_1","session_id":"sess_1","correlation_id":"model:run_1:0","payload":{"schema_version":"model.invocation.completed.v0","run_id":"run_1","invocation_id":"model:run_1:0","profile":"default","provider":"openai","model":"gpt-4o","started_at":"2026-07-15T09:59:50Z","finished_at":"2026-07-15T10:00:00Z","latency_ms":10000,"input_tokens":100,"cached_input_tokens":10,"output_tokens":200,"reasoning_tokens":5,"total_tokens":315,"finish_reason":"stop","error_category":null,"estimated_cost":0.01,"receipt_id":"model-receipt:model:run_1:0"}},
{"event_id":"evt_2","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T11:00:00Z","run_id":"run_1","principal_id":"principal_1","session_id":"sess_1","correlation_id":"model:run_1:1","payload":{"schema_version":"model.invocation.completed.v0","run_id":"run_1","invocation_id":"model:run_1:1","profile":"default","provider":"openai","model":"gpt-4o","started_at":"2026-07-15T10:59:55Z","finished_at":"2026-07-15T11:00:00Z","latency_ms":5000,"input_tokens":50,"cached_input_tokens":0,"output_tokens":30,"reasoning_tokens":0,"total_tokens":80,"finish_reason":"stop","error_category":null,"estimated_cost":0.005,"receipt_id":"model-receipt:model:run_1:1"}},
{"event_id":"evt_3","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T12:00:00Z","run_id":"run_2","principal_id":"principal_2","session_id":"sess_2","correlation_id":"model:run_2:0","payload":{"schema_version":"model.invocation.completed.v0","run_id":"run_2","invocation_id":"model:run_2:0","profile":"analyst","provider":"anthropic","model":"claude-3","started_at":"2026-07-15T11:59:50Z","finished_at":"2026-07-15T12:00:00Z","latency_ms":10000,"input_tokens":20,"cached_input_tokens":0,"output_tokens":10,"reasoning_tokens":0,"total_tokens":30,"finish_reason":"stop","error_category":null,"estimated_cost":0.002,"receipt_id":"model-receipt:model:run_2:0"}}
]}"#;

    let output = run_candidate(candidate_path, observe_page);

    if !output.contains("\"ok\":true") {
        eprintln!("FAIL: projection did not report ok=true");
        eprintln!("Raw output: {}", output.trim());
        std::process::exit(1);
    }

    struct Expectation {
        group: &'static str,
        key: &'static str,
        field: &'static str,
        value: i64,
    }

    let expectations = vec![
        Expectation { group: "by_date",    key: "2026-07-15", field: "total_tokens",       value: 425 },
        Expectation { group: "by_date",    key: "2026-07-15", field: "input_tokens",       value: 170 },
        Expectation { group: "by_date",    key: "2026-07-15", field: "cached_input_tokens", value: 10 },
        Expectation { group: "by_run",     key: "run_1",      field: "total_tokens",       value: 395 },
        Expectation { group: "by_run",     key: "run_2",      field: "total_tokens",       value: 30 },
        Expectation { group: "by_model",   key: "gpt-4o",     field: "total_tokens",       value: 395 },
        Expectation { group: "by_model",   key: "claude-3",   field: "total_tokens",       value: 30 },
        Expectation { group: "by_profile", key: "default",    field: "output_tokens",      value: 230 },
        Expectation { group: "by_profile", key: "analyst",    field: "input_tokens",       value: 20 },
    ];

    let mut failures = 0;
    for exp in &expectations {
        let needle = format!(r#""{}":{{"#, exp.key);
        let group_pos = match output.find(&format!("\"{}\":{{", exp.group)) {
            Some(pos) => pos,
            None => {
                eprintln!("FAIL: group '{}' not found", exp.group);
                failures += 1;
                continue;
            }
        };
        // Find the key within the group block (the key must appear after the group).
        let group_slice = &output[group_pos..];
        let key_pos = match group_slice.find(&needle) {
            Some(pos) => group_pos + pos,
            None => {
                eprintln!("FAIL: key '{}' not found in group '{}'", exp.key, exp.group);
                failures += 1;
                continue;
            }
        };
        let value = number_after(&output, key_pos + needle.len(), exp.field);
        match value {
            Some(actual) if actual == exp.value => {
                eprintln!(
                    "PASS: {}.{}.{} = {}",
                    exp.group, exp.key, exp.field, exp.value
                );
            }
            Some(actual) => {
                eprintln!(
                    "FAIL: {}.{}.{} expected {} got {}",
                    exp.group, exp.key, exp.field, exp.value, actual
                );
                failures += 1;
            }
            None => {
                eprintln!(
                    "FAIL: field '{}' not found for {}.{}",
                    exp.field, exp.group, exp.key
                );
                failures += 1;
            }
        }
    }

    if failures > 0 {
        eprintln!("\n{} test(s) FAILED", failures);
        std::process::exit(1);
    } else {
        eprintln!("\nAll {} assertion(s) PASSED", expectations.len());
        std::process::exit(0);
    }
}

fn run_candidate(candidate_path: &str, input: &str) -> String {
    let mut child = Command::new(candidate_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            eprint!("Failed to spawn candidate: {}", e);
            std::process::exit(1);
        });

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    let _ = child.stdout.take().unwrap().read_to_string(&mut stdout);
    let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);

    let status = child.wait().unwrap_or_else(|e| {
        eprint!("Failed to wait for candidate: {}", e);
        std::process::exit(1);
    });

    if !status.success() && stdout.is_empty() {
        eprint!(
            "Candidate exited code {:?}, stderr: {}",
            status.code(),
            stderr
        );
        std::process::exit(1);
    }

    stdout
}

fn number_after(raw: &str, start: usize, field: &str) -> Option<i64> {
    let marker = format!("\"{field}\":");
    let slice = raw.get(start..)?;
    let pos = slice.find(&marker)?;
    let after = slice.get(pos + marker.len()..)?.trim_start();
    let end = after
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+')))
        .unwrap_or(after.len());
    after.get(..end)?.parse().ok()
}
