use super::*;
use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
use agent_core_kernel::domain::DevelopmentRequestDraft;

const GOOD_SOURCE: &str = r#"
pub fn transform(upstream: &Value) -> Value {
    let failures = upstream
        .pointer("/rendered/failure_events")
        .and_then(Value::as_array);
    let Some(failures) = failures else {
        return json!({"status":"no_failures","source_component":"failure-viewer"});
    };
    let latest = failures.iter().max_by(|left, right| {
        let left = left.get("receipt_time").and_then(Value::as_str).unwrap_or("");
        let right = right.get("receipt_time").and_then(Value::as_str).unwrap_or("");
        left.cmp(right)
    });
    let Some(latest) = latest else {
        return json!({"status":"no_failures","source_component":"failure-viewer"});
    };
    json!({
        "capability_name": latest.get("capability_name").cloned().unwrap_or(Value::Null),
        "failed_stage": latest.get("failed_stage").cloned().unwrap_or(Value::Null),
        "error_category": latest.get("error_category").cloned().unwrap_or(Value::Null),
        "detail_code": latest.get("detail_code").cloned().unwrap_or(Value::Null),
        "run_id": latest.get("run_id").cloned().unwrap_or(Value::Null),
        "invocation_id": latest.get("invocation_id").cloned().unwrap_or(Value::Null),
        "receipt_status": latest.get("receipt_status").cloned().unwrap_or(Value::Null),
        "receipt_time": latest.get("receipt_time").cloned().unwrap_or(Value::Null),
        "source_component": "failure-viewer"
    })
}
"#;

fn request() -> DevelopmentRequest {
    let mut draft = DevelopmentRequestDraft::new(
        TargetKind::InvocableCapability,
        "external.failure_viewer_query".into(),
    );
    draft.requirements = vec!["query the current failure viewer state".into()];
    draft.required_contracts = vec!["component.invoke.v0".into()];
    draft.requested_permissions = vec!["component.invoke".into()];
    draft.acceptance_criteria = vec!["return the latest failure facts".into()];
    DevelopmentRequest::from_draft(
        draft,
        "principal:test".into(),
        "scope:test".into(),
        "message:test".into(),
        "development:message:test".into(),
        CONTRACT_CATALOG_VERSION.into(),
    )
    .unwrap()
}

#[test]
fn source_policy_rejects_host_access() {
    source::normalize(GOOD_SOURCE).unwrap();
    let unsafe_source = GOOD_SOURCE.replace(
        "pub fn transform(upstream: &Value) -> Value {",
        "pub fn transform(upstream: &Value) -> Value { let _ = std::fs::read(\"/etc/passwd\");",
    );
    assert_eq!(
        source::normalize(&unsafe_source).unwrap_err().code(),
        "GENERATOR_MODEL_OUTPUT_UNSAFE"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn frozen_candidate_passes_failure_and_empty_private_cases() {
    let base = std::env::temp_dir().join(format!(
        "invocable_generator_test_{}_{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let request = request();
    let result = verify_candidate(
        &base,
        "failure-viewer-query",
        &request,
        AcceptanceKitId::FailureViewerQueryV0,
        &source::normalize(GOOD_SOURCE).unwrap(),
        "failure-viewer",
        "/api/state",
    );
    let _ = std::fs::remove_dir_all(&base);
    assert!(result.is_ok(), "{result:?}");
}
