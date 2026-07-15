//! Opt-in real-model smoke for the request-driven HookConsumerService generator.
//!
//! Run only in an operator-controlled environment with the normal model
//! endpoint variables configured. Candidate code is compiled but never
//! deployed by this test.

use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
use agent_core_kernel::domain::{DevelopmentRequest, DevelopmentRequestDraft, TargetKind};
use coding_harness::self_evolution;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "requires configured real model endpoint"]
fn live_token_dashboard_model_generation() {
    let request_text = "开发一个 Token 使用量 Dashboard，\n通过 event.observe.v0 获取数据，\n按日期、Run、模型和 Profile 展示 Token 用量。";
    let mut draft =
        DevelopmentRequestDraft::new(TargetKind::HookConsumerService, "token-dashboard".into());
    draft.requirements = request_text
        .split(['，', '\n', '。'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    draft.required_contracts = vec!["event.observe.v0".into()];
    draft.requested_permissions = vec!["journal.observe".into()];
    draft.acceptance_criteria = draft.requirements.clone();
    let request = DevelopmentRequest::from_draft(
        draft,
        "principal:live-generator-test".into(),
        "scope:live-generator-test".into(),
        "message:live-generator-test".into(),
        "development:live-generator-test".into(),
        CONTRACT_CATALOG_VERSION.into(),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!(
        "generic_generator_live_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let response = self_evolution::handle_submit(&root, &json!({"development_request": request}));
    assert_eq!(response["ok"], true, "generation failed: {response}");
    let result = &response["result"];
    assert_eq!(
        result["component_manifest"]["generation"]["kind"],
        "request-driven-model-module-v0"
    );
    let candidate = root.join(result["candidate_ref"].as_str().unwrap());
    let mut build_command = Command::new("cargo");
    build_command.env_clear();
    for name in ["PATH", "HOME", "TMPDIR", "CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            build_command.env(name, value);
        }
    }
    let build = build_command
        .args(["build", "--release", "--locked"])
        .current_dir(&candidate)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "generated candidate did not compile: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let page = r#"{"schema_version":"event.observe.v0","next_cursor":3,"has_more":false,"events":[{"event_id":"one","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-one","payload":{"profile":"default","model":"model-a<img src=x onerror=alert(1)>","latency_ms":100,"input_tokens":10,"cached_input_tokens":2,"output_tokens":5,"reasoning_tokens":1,"total_tokens":16}},{"event_id":"two","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T11:00:00Z","run_id":"run-two","payload":{"profile":"analysis","model":"model-b","latency_ms":300,"input_tokens":20,"cached_input_tokens":3,"output_tokens":8,"reasoning_tokens":2,"total_tokens":30}},{"event_id":"three","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-15T12:00:00Z","run_id":"run-two","payload":{"profile":"analysis","model":"model-b","latency_ms":50,"error_category":"dependency_unavailable"}}]}"#;
    let binary = candidate.join("target/release/generated-hook-consumer");
    let mut candidate_command = Command::new(binary);
    candidate_command.env_clear();
    let mut child = candidate_command
        .arg("--profile-contract-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(page.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let rendered = String::from_utf8(output.stdout).unwrap().to_lowercase();
    for required in [
        "input",
        "cached",
        "output",
        "reasoning",
        "latency",
        "failure",
        "run-one",
        "model-a",
        "default",
        "30",
        "html_nonempty",
        "\"html_safe\":true",
        "\"html_runtime_metadata\":true",
        "\"html_telemetry_metrics\":true",
        "\"html_average_latency\":true",
    ] {
        assert!(
            rendered.contains(required),
            "generated dashboard omitted {required}: {rendered}"
        );
    }
    if std::env::var("CODING_GENERATOR_TEST_KEEP_CANDIDATE").as_deref() == Ok("1") {
        eprintln!("live generator candidate retained at {}", root.display());
    } else {
        let _ = std::fs::remove_dir_all(root);
    }
}
