use super::*;
use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
use agent_core_kernel::domain::{DevelopmentRequestDraft, TargetKind};
use std::io::{Read, Write};
use std::net::TcpListener;

fn request() -> DevelopmentRequest {
    let mut draft =
        DevelopmentRequestDraft::new(TargetKind::HookConsumerService, "observer-ui".into());
    draft.requirements = vec!["render observed facts".into()];
    draft.required_contracts = vec!["event.observe.v0".into()];
    draft.requested_permissions = vec!["journal.observe".into()];
    draft.acceptance_criteria = vec!["render a read-only page".into()];
    DevelopmentRequest::from_draft(
        draft,
        "principal:test".into(),
        "scope:test".into(),
        "message:test".into(),
        "development:test".into(),
        CONTRACT_CATALOG_VERSION.into(),
    )
    .unwrap()
}

fn safe_source() -> &'static str {
    r#"pub fn initial_state() -> Value { json!({}) }
pub fn apply_event(state: &mut Value, event: &Value) { state["last"] = event.clone(); }
pub fn render_json(state: &Value, runtime: &Value) -> Value { json!({"state":state,"runtime":runtime}) }
pub fn render_html(state: &Value, runtime: &Value) -> String { let _ = (state, runtime); "<h1>observer</h1>".to_string() }"#
}

#[test]
fn source_policy_rejects_host_access_and_extra_public_api() {
    validate_generated_source(safe_source()).unwrap();
    assert_eq!(
        validate_generated_source(&format!(
            "{}\nfn steal() {{ std::fs::read(\"/etc/passwd\").unwrap(); }}",
            safe_source()
        ))
        .unwrap_err()
        .code(),
        "GENERATOR_MODEL_OUTPUT_UNSAFE"
    );
    assert!(
        validate_generated_source(&format!("{}\npub fn hidden() {{}}", safe_source())).is_err()
    );
    assert!(validate_generated_source(&format!("{}\npub struct Hidden;", safe_source())).is_err());
    for bypass in [
        r#"fn steal() { let _ = r#std::fs::read("/etc/passwd"); }"#,
        r#"fn steal() { let _ = super::required_env("EVENT_OBSERVE_TOKEN"); }"#,
        r#"fn steal() { use super::required_env; let _ = required_env("EVENT_OBSERVE_TOKEN"); }"#,
        r#"fn steal() { include_str!("/etc/passwd"); }"#,
        r#"fn steal() -> Value { json!({"secret": super :: required_env("EVENT_OBSERVE_TOKEN")}) }"#,
        r#"fn steal() -> String { format!("{}", r#std :: fs :: read_to_string("/etc/passwd").unwrap()) }"#,
        r#"fn steal() -> Value { json!({"secret": include_str!("/etc/passwd")}) }"#,
    ] {
        assert_eq!(
            validate_generated_source(&format!("{}\n{bypass}", safe_source()))
                .unwrap_err()
                .code(),
            "GENERATOR_MODEL_OUTPUT_UNSAFE"
        );
    }
}

#[test]
fn normalization_strips_allowed_imports_and_rejects_inner_crate_access() {
    let imported = format!(
        "use serde_json::{{json, Value}};\nuse crate::support::html_escape;\n{}",
        safe_source()
    );
    let normalized = normalize_generated_source(&imported).unwrap();
    assert!(!normalized.contains("use "));
    assert!(validate_generated_source(&normalized).is_ok());

    let inner_import = safe_source().replace(
        "let _ = (state, runtime);",
        "use crate::support::html_escape; let _ = (state, runtime);",
    );
    assert_eq!(
        normalize_generated_source(&inner_import)
            .unwrap_err()
            .code(),
        "GENERATOR_MODEL_OUTPUT_UNSAFE"
    );
}

#[test]
fn fenced_source_is_extracted_even_with_provider_commentary() {
    let wrapped = format!(
        "I will provide the module.\n```rust\n{}\n```\nDone.",
        safe_source()
    );
    assert_eq!(strip_markdown_fence(&wrapped), safe_source());
}

#[test]
fn initial_generation_retries_only_discardable_model_failures() {
    for code in [
        "GENERATOR_MODEL_UNAVAILABLE",
        "GENERATOR_MODEL_RESPONSE_INVALID",
        "GENERATOR_MODEL_OUTPUT_TRUNCATED",
        "GENERATOR_MODEL_OUTPUT_INVALID",
        "GENERATOR_MODEL_OUTPUT_INVALID_RUST",
        "GENERATOR_MODEL_OUTPUT_UNSAFE",
        "GENERATOR_MODEL_OUTPUT_INTERFACE_MISMATCH",
    ] {
        assert!(retry::retryable_model_output_error(code));
    }
    assert!(!retry::retryable_model_output_error(
        "GENERATOR_MODEL_NOT_CONFIGURED"
    ));
}

#[test]
fn rejected_repair_output_uses_the_remaining_shared_attempt() {
    let mut calls = 0;
    let (source, attempts) = retry::retry_model_output(2, || {
        calls += 1;
        if calls == 1 {
            Err(GenerationError::new(
                "GENERATOR_MODEL_OUTPUT_INTERFACE_MISMATCH",
            ))
        } else {
            Ok(safe_source().to_string())
        }
    })
    .unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(calls, 2);
    assert_eq!(source, safe_source());
}

#[test]
fn initial_generation_accepts_a_safe_third_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let unsafe_source = format!(
        "{}\nfn steal() {{ let _ = std::fs::read(\"/etc/passwd\"); }}",
        safe_source()
    );
    let safe_response_source = safe_source().to_string();
    let server = std::thread::spawn(move || {
        let responses = [
            "not-json".to_string(),
            json!({"choices":[{"message":{"content":unsafe_source}}]}).to_string(),
            json!({"choices":[{"message":{"content":safe_response_source}}]}).to_string(),
        ];
        for body in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 16 * 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    let (generated, attempts) =
        generate_module_with_retry(&ModelConfig::for_test(endpoint), &request())
            .expect("the third safe response should be accepted");
    server.join().unwrap();
    assert_eq!(attempts, 3);
    assert_eq!(
        generated,
        normalize_generated_source(safe_source()).unwrap()
    );
}

fn private_required_source() -> &'static str {
    r#"fn initial_state() -> Value { json!({}) }
fn apply_event(state: &mut Value, event: &Value) { state["last"] = event.clone(); }
fn render_json(state: &Value, runtime: &Value) -> Value { json!({"state":state,"runtime":runtime}) }
fn render_html(state: &Value, runtime: &Value) -> String { let _ = (state, runtime); "<h1>observer</h1>".to_string() }"#
}

#[test]
fn normalizer_promotes_required_functions_to_public() {
    let normalized = normalize_generated_source(private_required_source()).unwrap();
    // Must contain `pub fn` for all four required functions
    assert!(normalized.contains("pub fn initial_state"));
    assert!(normalized.contains("pub fn apply_event"));
    assert!(normalized.contains("pub fn render_json"));
    assert!(normalized.contains("pub fn render_html"));
    // Must pass the strict interface validation
    validate_generated_source(&normalized).unwrap();
}

#[test]
fn normalizer_does_not_promote_helper_functions() {
    let src = format!(
        r#"{}
fn helper() -> u32 {{ 42 }}"#,
        private_required_source()
    );
    let normalized = normalize_generated_source(&src).unwrap();
    // Helper stays private
    assert!(!normalized.contains("pub fn helper"));
    // Required functions are promoted
    assert!(normalized.contains("pub fn initial_state"));
    validate_generated_source(&normalized).unwrap();
}

#[test]
fn normalizer_does_not_create_missing_required_function() {
    let src = r#"pub fn initial_state() -> Value { json!({}) }
pub fn apply_event(state: &mut Value, event: &Value) { state["last"] = event.clone(); }
pub fn render_json(state: &Value, runtime: &Value) -> Value { json!({"state":state,"runtime":runtime}) }"#;
    // Missing render_html — should fail even after normalization
    assert_eq!(
        normalize_generated_source(src).unwrap_err().code(),
        "GENERATOR_MODEL_OUTPUT_INTERFACE_MISMATCH"
    );
}

#[test]
fn normalizer_does_not_hide_wrong_function_name() {
    let src = r#"pub fn initial_state() -> Value { json!({}) }
pub fn apply_event(state: &mut Value, event: &Value) { state["last"] = event.clone(); }
pub fn render_json(state: &Value, runtime: &Value) -> Value { json!({"state":state,"runtime":runtime}) }
pub fn render_watcher(state: &Value, runtime: &Value) -> String { "<h1>observer</h1>".to_string() }"#;
    // render_watcher is not render_html — name mismatch, strict validation fails
    assert_eq!(
        normalize_generated_source(src).unwrap_err().code(),
        "GENERATOR_MODEL_OUTPUT_INTERFACE_MISMATCH"
    );
}

#[test]
fn normalizer_rejects_extra_public_function() {
    let src = r#"pub fn initial_state() -> Value { json!({}) }
pub fn apply_event(state: &mut Value, event: &Value) { state["last"] = event.clone(); }
pub fn render_json(state: &Value, runtime: &Value) -> Value { json!({"state":state,"runtime":runtime}) }
pub fn render_html(state: &Value, runtime: &Value) -> String { "<h1>observer</h1>".to_string() }
pub fn extra() -> String { "extra".to_string() }"#;
    // Extra public function should fail strict validation
    assert_eq!(
        normalize_generated_source(src).unwrap_err().code(),
        "GENERATOR_MODEL_OUTPUT_INTERFACE_MISMATCH"
    );
}

#[test]
fn normalizer_preserves_function_bodies() {
    let normalized = normalize_generated_source(private_required_source()).unwrap();
    // Body content preserved after normalization
    assert!(normalized.contains("json!({})"));
    assert!(normalized.contains("state[\"last\"] = event.clone()"));
    assert!(normalized.contains("<h1>observer</h1>"));
}

#[test]
fn normalized_realistic_model_output_passes_interface_validation() {
    // This is structurally equivalent to the real deepseek-v4-flash model output
    // from the canary run — all four required functions exist but lack `pub`.
    let realistic_output = r#"fn initial_state() -> Value {
    json!({"failures": {"list": []}})
}

fn apply_event(state: &mut Value, event: &Value) {
    let kind = match value_string(event, &["event_kind"]) {
        Some(k) => k,
        None => return,
    };
    if !kind.contains("failure") && kind != "failure" {
        return;
    }
    let timestamp = value_display(event, &["payload", "timestamp"])
        .unwrap_or_else(|| "unknown".to_string());
    let error_msg = value_display(event, &["payload", "error"])
        .unwrap_or_else(|| "no error".to_string());
    let failures_map = ensure_object_path(state, &["failures"]);
    let list_val = failures_map.entry("list").or_insert(Value::Array(Vec::new()));
    if let Value::Array(ref mut list) = list_val {
        const MAX: usize = 100;
        while list.len() >= MAX { list.remove(0); }
        list.push(json!({"time": timestamp, "type": kind, "error": error_msg}));
    }
}

fn render_json(state: &Value, _runtime: &Value) -> Value {
    match state.get("failures").and_then(|v| v.get("list")) {
        Some(list) => list.clone(),
        None => json!([]),
    }
}

fn render_html(state: &Value, _runtime: &Value) -> String {
    let failures = state.get("failures").and_then(|v| v.get("list"))
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if failures.is_empty() {
        return "<div class=\"failures\"><p>No recent failures.</p></div>".to_string();
    }
    let rows: String = failures.iter().map(|f| {
        let time = f.get("time").and_then(|v| v.as_str()).unwrap_or("unknown");
        let kind = f.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        let error = f.get("error").and_then(|v| v.as_str()).unwrap_or("no error");
        format!("<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_escape(time), html_escape(kind), html_escape(error))
    }).collect();
    format!("<table class=\"failures\"><thead><tr><th>Time</th><th>Type</th><th>Error</th></tr></thead><tbody>{}</tbody></table>", rows)
}"#;
    let normalized = normalize_generated_source(realistic_output).unwrap();
    assert!(normalized.contains("pub fn initial_state"));
    assert!(normalized.contains("pub fn apply_event"));
    assert!(normalized.contains("pub fn render_json"));
    assert!(normalized.contains("pub fn render_html"));
    validate_generated_source(&normalized).unwrap();
}

#[test]
fn model_response_is_bounded_to_the_single_module() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let source = safe_source().to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let text = String::from_utf8_lossy(&request);
        assert!(text.contains("DEVELOPMENT_REQUEST_JSON_BEGIN"));
        assert!(!text.contains("principal:test"));
        let body = json!({"choices":[{"message":{"content":format!("```rust\n{source}\n```")}}]})
            .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    let generated = generate_module(&ModelConfig::for_test(endpoint), &request()).unwrap();
    server.join().unwrap();
    assert_eq!(
        generated,
        normalize_generated_source(safe_source()).unwrap()
    );
}
