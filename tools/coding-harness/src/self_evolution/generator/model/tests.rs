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

#[test]
fn retry_exhaustion_returns_last_discardable_error() {
    // Test the retry logic directly without a mock server
    let mut calls = 0;
    let error = retry::retry_model_output(3, || {
        calls += 1;
        Err(GenerationError::new("GENERATOR_MODEL_RESPONSE_INVALID"))
    })
    .unwrap_err();
    assert_eq!(error.code(), "GENERATOR_MODEL_RESPONSE_INVALID");
    assert_eq!(calls, 3, "should exhaust all 3 retries");
}

#[test]
fn retry_unsafe_output_stops_early_and_does_not_retry() {
    // Unsafe output should NOT be retried
    let mut calls = 0;
    let error = retry::retry_model_output(3, || {
        calls += 1;
        Err(GenerationError::new("GENERATOR_MODEL_OUTPUT_UNSAFE"))
    })
    .unwrap_err();
    assert_eq!(error.code(), "GENERATOR_MODEL_OUTPUT_UNSAFE");
    assert_eq!(calls, 1, "unsafe should stop immediately without retry");
}

#[test]
fn retry_truncated_output_uses_remaining_budget() {
    let mut calls = 0;
    let (source, attempts) = retry::retry_model_output(3, || {
        calls += 1;
        if calls < 3 {
            Err(GenerationError::new("GENERATOR_MODEL_OUTPUT_TRUNCATED"))
        } else {
            Ok(safe_source().to_string())
        }
    })
    .expect("third attempt should succeed");
    assert_eq!(attempts, 3);
    assert_eq!(calls, 3);
    assert_eq!(source, safe_source());
}

#[test]
fn no_progress_same_error_continues_retrying() {
    // Same error repeated should exhaust retries (all retryable)
    let mut calls = 0;
    let error = retry::retry_model_output(2, || {
        calls += 1;
        Err(GenerationError::new("GENERATOR_MODEL_UNAVAILABLE"))
    })
    .unwrap_err();
    assert_eq!(calls, 2, "same retryable error should use all retries");
    assert_eq!(error.code(), "GENERATOR_MODEL_UNAVAILABLE");
}
