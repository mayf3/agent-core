use super::*;
use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
use agent_core_kernel::domain::DevelopmentRequestDraft;
use serde_json::json;

const LENGTH_SOURCE: &str = r#"
pub fn invoke(arguments: &Value) -> Value {
    let length = arguments
        .get("text")
        .and_then(Value::as_str)
        .map(|text| text.chars().count() as u64)
        .unwrap_or(0);
    json!({"length": length})
}
"#;

const WORD_SOURCE: &str = r#"
pub fn invoke(arguments: &Value) -> Value {
    let count = arguments
        .get("text")
        .and_then(Value::as_str)
        .map(|text| text.split_whitespace().count() as u64)
        .unwrap_or(0);
    json!({"words": count})
}
"#;

fn request(name: &str, requirement: &str) -> DevelopmentRequest {
    let mut draft = DevelopmentRequestDraft::new(TargetKind::InvocableCapability, name.into());
    draft.requirements = vec![requirement.into()];
    draft.required_contracts = vec!["component.invoke.v0".into()];
    draft.requested_permissions = vec!["component.invoke".into()];
    draft.acceptance_criteria = vec!["the declared schema and contract cases pass".into()];
    DevelopmentRequest::from_draft(
        draft,
        "principal:test".into(),
        "scope:test".into(),
        format!("message:{name}"),
        format!("development:{name}"),
        CONTRACT_CATALOG_VERSION.into(),
    )
    .unwrap()
}

fn length_contract() -> CapabilityContract {
    CapabilityContract {
        description: "Count Unicode scalar values in a string.".into(),
        input_schema: json!({
            "type":"object",
            "properties":{"text":{"type":"string"}},
            "required":["text"],
            "additionalProperties":false
        }),
        output_schema: json!({
            "type":"object",
            "properties":{"length":{"type":"integer","minimum":0}},
            "required":["length"],
            "additionalProperties":false
        }),
        probe_arguments: json!({"text":"你好abc"}),
        probe_result: json!({"length":5}),
        contract_tests: vec![
            contract::ContractCase {
                case_id: "unicode-mixed".into(),
                arguments: json!({"text":"你好abc"}),
                expected_result: json!({"length":5}),
            },
            contract::ContractCase {
                case_id: "emoji".into(),
                arguments: json!({"text":"🙂a"}),
                expected_result: json!({"length":2}),
            },
        ],
    }
}

fn word_contract() -> CapabilityContract {
    CapabilityContract {
        description: "Count whitespace-separated words.".into(),
        input_schema: json!({
            "type":"object",
            "properties":{"text":{"type":"string"}},
            "required":["text"],
            "additionalProperties":false
        }),
        output_schema: json!({
            "type":"object",
            "properties":{"words":{"type":"integer","minimum":0}},
            "required":["words"],
            "additionalProperties":false
        }),
        probe_arguments: json!({"text":"one two"}),
        probe_result: json!({"words":2}),
        contract_tests: vec![
            contract::ContractCase {
                case_id: "two-words".into(),
                arguments: json!({"text":"one two"}),
                expected_result: json!({"words":2}),
            },
            contract::ContractCase {
                case_id: "empty".into(),
                arguments: json!({"text":""}),
                expected_result: json!({"words":0}),
            },
        ],
    }
}

#[test]
fn two_pure_compute_requests_materialize_distinct_implementations() {
    let root = std::env::temp_dir().join(format!(
        "invocable_generator_distinct_{}_{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let length = materialize(
        &root,
        "length-candidate",
        &request("external.unicode_length", "count Unicode characters"),
        &length_contract(),
        &source::normalize(LENGTH_SOURCE).unwrap(),
        "test-model",
    )
    .unwrap();
    let words = materialize(
        &root,
        "word-candidate",
        &request("external.word_count", "count whitespace-separated words"),
        &word_contract(),
        &source::normalize(WORD_SOURCE).unwrap(),
        "test-model",
    )
    .unwrap();
    assert_ne!(length["candidate_digest"], words["candidate_digest"]);
    assert_ne!(
        std::fs::read_to_string(root.join("length-candidate/candidate/src/component.rs")).unwrap(),
        std::fs::read_to_string(root.join("word-candidate/candidate/src/component.rs")).unwrap()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn schemas_probe_and_unicode_behavior_are_bound() {
    let contract = length_contract();
    contract.validate().unwrap();
    let source = source::normalize(LENGTH_SOURCE).unwrap();
    assert!(source.contains(".chars().count()"));
    let manifest = cache::component_manifest(
        &request("external.unicode_length", "count Unicode characters"),
        &format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes()))),
        "test-model",
        &contract,
    );
    assert_eq!(
        manifest["capability"]["input_schema"],
        contract.input_schema
    );
    assert_eq!(
        manifest["capability"]["output_schema"],
        contract.output_schema
    );
    assert_eq!(
        contract.probe_result,
        json!({"length": "你好abc".chars().count()})
    );
}

#[test]
fn source_policy_rejects_host_access_and_extra_public_api() {
    source::normalize(LENGTH_SOURCE).unwrap();
    let unsafe_source = LENGTH_SOURCE.replace(
        "pub fn invoke(arguments: &Value) -> Value {",
        "pub fn invoke(arguments: &Value) -> Value { let _ = std::fs::read(\"/etc/passwd\");",
    );
    assert_eq!(
        source::normalize(&unsafe_source).unwrap_err().code(),
        "GENERATOR_MODEL_OUTPUT_UNSAFE"
    );
    assert!(source::normalize(&format!(
        "{LENGTH_SOURCE}\npub fn second(_: &Value) -> Value {{ json!(null) }}"
    ))
    .is_err());
}

#[test]
fn contract_rejects_unbound_probe_and_schema_mismatch() {
    let mut unbound = length_contract();
    unbound.probe_result = json!({"length":99});
    assert_eq!(
        unbound.validate().unwrap_err().code(),
        "GENERATOR_PROFILE_CONTRACT_INVALID"
    );
    let mut bad_schema = length_contract();
    bad_schema.input_schema["additionalProperties"] = json!(true);
    assert_eq!(
        bad_schema.validate().unwrap_err().code(),
        "GENERATOR_SCHEMA_INVALID"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn generated_unicode_candidate_compiles_and_passes_profile_contract() {
    let base = std::env::temp_dir().join(format!(
        "invocable_generator_verify_{}_{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let result = verify_candidate(
        &base,
        "unicode-length",
        &request("external.unicode_length", "count Unicode characters"),
        &length_contract(),
        &source::normalize(LENGTH_SOURCE).unwrap(),
    );
    let _ = std::fs::remove_dir_all(base);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
#[cfg(target_os = "linux")]
fn five_gates_bind_stable_artifact_digest_into_acceptance_receipt() {
    let root = std::env::temp_dir().join(format!(
        "invocable_generator_acceptance_{}_{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let request = request("external.unicode_length", "count Unicode characters");
    let generated = materialize(
        &root.join("generated"),
        "unicode-acceptance",
        &request,
        &length_contract(),
        &source::normalize(LENGTH_SOURCE).unwrap(),
        "test-model",
    )
    .unwrap();
    let args = json!({"invocation_intent_id":"invocation-test"});
    let first =
        crate::self_evolution::profile_acceptance::accept(&root, &args, &request, &generated)
            .unwrap();
    let replay =
        crate::self_evolution::profile_acceptance::accept(&root, &args, &request, &generated)
            .unwrap();
    assert_eq!(first, replay);
    assert_eq!(first["acceptance_outcome"], "passed");
    assert_eq!(
        first["artifact_digest"],
        first["acceptance_receipt"]["subject_digest"]
    );
    assert!(first["artifact_digest"]
        .as_str()
        .is_some_and(|value| value.starts_with("sha256:")));
    let evidence_digest = first["evidence_digest"].as_str().unwrap();
    let store = agent_core_kernel::capabilities::store::ContentStore::new(root.clone());
    let evidence = store
        .load(
            &agent_core_kernel::capabilities::store::Sha256Digest::parse(evidence_digest).unwrap(),
        )
        .unwrap();
    let evidence: Value = serde_json::from_slice(&evidence).unwrap();
    assert_eq!(evidence["gate_results"].as_array().unwrap().len(), 5);
    assert!(evidence["gate_results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|gate| gate["passed"] == true));
    let _ = std::fs::remove_dir_all(root);
}
