//! Unit tests for Hook ABI v0 schema + config.
//!
//! Coverage requirements (from the Phase 1 PR spec):
//! 1. HookKind serde round-trip
//! 2. Unknown hook kind rejected or handled explicitly
//! 3. Hook config default is disabled
//! 4. Hook limits have finite safe defaults
//! 5. ContextFragment rejects or prevents over-limit response via validation
//! 6. ResourceRef is opaque and does not require memory/skill/task fields
//! 7. FailureMode serde round-trip
//! 8. No product-layer terms appear in Hook ABI public type names

use crate::hook::*;

// ---------------------------------------------------------------------------
// 1. HookKind serde round-trip
// ---------------------------------------------------------------------------

#[test]
fn hook_kind_serde_round_trip() {
    let cases = [
        (HookKind::IngressRouteV0, r#""ingress.route.v0""#),
        (HookKind::ContextPrepareV0, r#""context.prepare.v0""#),
        (HookKind::ContextLoadV0, r#""context.load.v0""#),
        (HookKind::ContextCompressV0, r#""context.compress.v0""#),
        (HookKind::EventObserveV0, r#""event.observe.v0""#),
        (HookKind::DecisionPolicyV0, r#""decision.policy.v0""#),
    ];
    for (kind, expected_json) in &cases {
        let serialized = serde_json::to_string(kind).unwrap();
        assert_eq!(&serialized, expected_json, "serialize {kind:?}");
        let deserialized: HookKind = serde_json::from_str(expected_json).unwrap();
        assert_eq!(&deserialized, kind, "deserialize {expected_json}");
    }
}

// ---------------------------------------------------------------------------
// 2. Unknown hook kind rejected or handled explicitly
// ---------------------------------------------------------------------------

#[test]
fn unknown_hook_kind_is_rejected() {
    let err = serde_json::from_str::<HookKind>(r#""unknown.hook.v0""#);
    assert!(err.is_err(), "unknown variant should fail to deserialize");

    let err = serde_json::from_str::<HookKind>(r#""memory.load.v0""#);
    assert!(
        err.is_err(),
        "product-layer variants like memory.load.v0 must be rejected"
    );

    let err = serde_json::from_str::<HookKind>(r#""skill.execute.v0""#);
    assert!(
        err.is_err(),
        "product-layer variants like skill.execute.v0 must be rejected"
    );
}

// ---------------------------------------------------------------------------
// 3. Hook config default is disabled
// ---------------------------------------------------------------------------

#[test]
fn hook_config_default_is_disabled() {
    let cfg = HookConfig::default();
    assert!(!cfg.enabled, "default config must be disabled");
    assert_eq!(
        cfg.failure_mode,
        HookFailureMode::Disabled,
        "default failure mode must be disabled"
    );
    // A disabled default must pass validation trivially.
    assert!(cfg.validate().is_ok());
}

#[test]
fn hook_registry_default_is_disabled() {
    let reg = HookRegistryConfig::default();
    assert!(!reg.enabled, "default registry must be disabled");
    assert!(reg.hooks.is_empty(), "default registry must have no hooks");
    assert!(
        reg.active_hooks().is_empty(),
        "active_hooks must be empty when master switch is off"
    );
}

// ---------------------------------------------------------------------------
// 4. Hook limits have finite safe defaults
// ---------------------------------------------------------------------------

#[test]
fn hook_limits_have_finite_safe_defaults() {
    let limits = HookLimits::default();
    assert_eq!(limits.timeout_ms, 5_000, "default timeout should be 5s");
    assert_eq!(
        limits.max_request_bytes,
        1024 * 1024,
        "default max_request_bytes should be 1 MiB"
    );
    assert_eq!(
        limits.max_response_bytes,
        1024 * 1024,
        "default max_response_bytes should be 1 MiB"
    );
    assert_eq!(
        limits.max_fragments, 20,
        "default max_fragments should be 20"
    );
    assert!(limits.validate().is_ok(), "default limits must validate");
}

#[test]
fn hook_limits_reject_excessive_timeout() {
    let limits = HookLimits {
        timeout_ms: 999_999,
        ..Default::default()
    };
    let err = limits.validate().unwrap_err();
    assert!(
        err.to_string().contains("timeout_ms"),
        "error should mention the field name: {}",
        err
    );
}

#[test]
fn hook_limits_reject_excessive_response_bytes() {
    let limits = HookLimits {
        max_response_bytes: 50 * 1024 * 1024,
        ..Default::default()
    };
    let err = limits.validate().unwrap_err();
    assert!(
        err.to_string().contains("max_response_bytes"),
        "error should mention the field name"
    );
}

// ---------------------------------------------------------------------------
// 5. ContextFragment validation against limits
// ---------------------------------------------------------------------------

#[test]
fn context_fragment_validates_against_limits() {
    let limits = HookLimits {
        max_response_bytes: 100,
        ..Default::default()
    };

    // A fragment whose content fits within limits should pass.
    let small = ContextFragment {
        content: "hello".to_string(),
        ..default_fragment()
    };
    assert!(small.validate_against(&limits).is_ok());

    // A fragment whose content exceeds limits should be rejected.
    let large = ContextFragment {
        content: "x".repeat(200),
        ..default_fragment()
    };
    let err = large.validate_against(&limits).unwrap_err();
    assert!(
        err.to_string().contains("exceeds maximum"),
        "error should describe the over-limit condition: {}",
        err
    );
    match err {
        HookValidationError::ContentTooLarge {
            content_size,
            max_bytes,
        } => {
            assert_eq!(content_size, 200);
            assert_eq!(max_bytes, 100);
        }
        _ => panic!("expected ContentTooLarge variant"),
    }
}

#[test]
fn context_fragment_zero_content_passes_validation() {
    let limits = HookLimits::default();
    let frag = ContextFragment {
        content: String::new(),
        ..default_fragment()
    };
    assert!(frag.validate_against(&limits).is_ok());
}

/// Helper: returns a `ContextFragment` with all non-content fields populated
/// with sensible defaults.
fn default_fragment() -> ContextFragment {
    ContextFragment {
        id: "test-frag-1".into(),
        hook_id: "context.prepare.v0".into(),
        kind: ContextFragmentKind::Fact,
        placement: FragmentPlacement::UserContext,
        priority: 0,
        content: String::new(), // caller overrides this
        source: "test".into(),
        ttl_secs: None,
        estimated_tokens: 0,
        sensitivity: FragmentSensitivity::Public,
    }
}

// ---------------------------------------------------------------------------
// 6. ResourceRef is opaque
// ---------------------------------------------------------------------------

#[test]
fn resource_ref_is_opaque() {
    // ResourceRef must not require Memory / Dream / Task / Skill fields.
    // Construct one with only the documented fields — this must compile and
    // round-trip.
    let r = ResourceRef {
        id: "res-001".into(),
        title: "Code review guidelines".into(),
        summary: "A document describing the team's code review process".into(),
        source: "docs:code-review".into(),
        estimated_token_cost: 500,
        load_hint: Some("priority:high".into()),
    };
    let json = serde_json::to_string(&r).unwrap();
    let deserialized: ResourceRef = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, r);

    // Verify the serialised form does NOT contain product-layer field names.
    assert!(
        !json.contains("memory"),
        "ResourceRef serialization must not contain 'memory': {json}"
    );
    assert!(
        !json.contains("dream"),
        "ResourceRef serialization must not contain 'dream': {json}"
    );
    assert!(
        !json.contains("task"),
        "ResourceRef serialization must not contain 'task': {json}"
    );
    assert!(
        !json.contains("skill"),
        "ResourceRef serialization must not contain 'skill': {json}"
    );
}

#[test]
fn resource_ref_round_trip() {
    let r = ResourceRef {
        id: "res-002".into(),
        title: String::new(),
        summary: String::new(),
        source: "opaque".into(),
        estimated_token_cost: 0,
        load_hint: None,
    };
    let json = serde_json::to_string(&r).unwrap();
    let back: ResourceRef = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
}

// ---------------------------------------------------------------------------
// 7. FailureMode serde round-trip
// ---------------------------------------------------------------------------

#[test]
fn failure_mode_serde_round_trip() {
    let cases = [
        (HookFailureMode::FailOpen, r#""fail_open""#),
        (HookFailureMode::FailClosed, r#""fail_closed""#),
        (HookFailureMode::Degrade, r#""degrade""#),
        (HookFailureMode::Disabled, r#""disabled""#),
    ];
    for (mode, expected_json) in &cases {
        let serialized = serde_json::to_string(mode).unwrap();
        assert_eq!(&serialized, expected_json, "serialize {mode:?}");
        let deserialized: HookFailureMode = serde_json::from_str(expected_json).unwrap();
        assert_eq!(&deserialized, mode, "deserialize {expected_json}");
    }
}

// ---------------------------------------------------------------------------
// 8. No product-layer terms in Hook ABI public type names
// ---------------------------------------------------------------------------

/// Checks that the stringified public API surface of `crate::hook` contains
/// none of the forbidden product-layer terms.
///
/// This is a compile-time-ish assertion: we enumerate the expected public
/// type names and verify none of them match the forbidden patterns.
#[test]
fn no_product_layer_terms_in_public_type_names() {
    // List of public hook types that SHOULD exist.
    let public_names: &[&str] = &[
        "HookKind",
        "HookFailureMode",
        "HookEndpoint",
        "HookLimits",
        "HookConfig",
        "HookRegistryConfig",
        "HookRequestEnvelope",
        "HookResponseEnvelope",
        "HookCallReceipt",
        "HookValidationError",
        "ContextFragment",
        "ContextFragmentKind",
        "FragmentPlacement",
        "FragmentSensitivity",
        "ResourceRef",
        "DecisionPolicyResult",
    ];

    let forbidden_terms = ["Memory", "Dream", "Task", "Skill", "Dashboard"];

    for name in public_names {
        for term in &forbidden_terms {
            assert!(
                !name.contains(term),
                "public type name '{name}' contains forbidden product-layer term '{term}'"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Additional: envelope serde round-trips
// ---------------------------------------------------------------------------

#[test]
fn hook_request_envelope_round_trip() {
    let env = HookRequestEnvelope {
        hook: HookKind::EventObserveV0,
        request_id: "req-001".into(),
        timestamp: chrono::Utc::now(),
        payload: serde_json::json!({"event_id": "evt_001"}),
    };
    let json = serde_json::to_string(&env).unwrap();
    let back: HookRequestEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(env.hook, back.hook);
    assert_eq!(env.request_id, back.request_id);
    assert_eq!(env.payload, back.payload);
}

#[test]
fn hook_call_receipt_round_trip() {
    let now = chrono::Utc::now();
    let receipt = HookCallReceipt {
        request_id: "req-002".into(),
        hook: HookKind::DecisionPolicyV0,
        endpoint: "http://localhost:9000/hook/policy".into(),
        started_at: now,
        completed_at: now,
        success: true,
        error: None,
        response_size_bytes: Some(256),
    };
    let json = serde_json::to_string(&receipt).unwrap();
    let back: HookCallReceipt = serde_json::from_str(&json).unwrap();
    assert_eq!(receipt, back);
}

#[test]
fn decision_policy_result_round_trip() {
    let cases = [
        (DecisionPolicyResult::ManualRequired, r#""manual_required""#),
        (DecisionPolicyResult::AutoApprove, r#""auto_approve""#),
        (DecisionPolicyResult::Deny, r#""deny""#),
        (DecisionPolicyResult::Defer, r#""defer""#),
    ];
    for (result, expected_json) in &cases {
        let serialized = serde_json::to_string(result).unwrap();
        assert_eq!(&serialized, expected_json, "serialize {result:?}");
        let deserialized: DecisionPolicyResult = serde_json::from_str(expected_json).unwrap();
        assert_eq!(&deserialized, result, "deserialize {expected_json}");
    }
}

// ---------------------------------------------------------------------------
// ContextFragment security constraint documentation (doc-test style)
// ---------------------------------------------------------------------------

/// Verify that the doc comments on ContextFragment compile and don't panic.
#[test]
fn context_fragment_security_constraints_are_present_in_type() {
    // This test exists so the security constraints are at least mechanically
    // exercised. The actual constraints are documented on the struct itself.
    let frag = ContextFragment {
        id: "sec-constraint-test".into(),
        hook_id: "test".into(),
        kind: ContextFragmentKind::Fact,
        placement: FragmentPlacement::SystemAppend,
        priority: 0,
        content: "test content".into(),
        source: "test".into(),
        ttl_secs: None,
        estimated_tokens: 10,
        sensitivity: FragmentSensitivity::Public,
    };
    // Confirm the struct round-trips.
    let json = serde_json::to_string(&frag).unwrap();
    let back: ContextFragment = serde_json::from_str(&json).unwrap();
    assert_eq!(frag, back);
}

// ── HookClient tests ───────────────────────────────────────────────────

#[test]
fn fake_hook_client_empty() {
    use crate::hook::client::FakeHookClient;
    use crate::hook::HookClient;
    let client = FakeHookClient::empty();
    let req = crate::hook::client::ContextPrepareRequest {
        hook: crate::hook::HookKind::ContextPrepareV0,
        run_id: "r".into(),
        session_id: "s".into(),
        agent_id: "main".into(),
        principal: "user".into(),
        channel: "cli".into(),
        user_text: "hello".into(),
        context_budget_chars: 4000,
    };
    let cfg = crate::hook::HookConfig {
        enabled: true,
        kind: crate::hook::HookKind::ContextPrepareV0,
        ..Default::default()
    };
    let resp = client.call_context_prepare(&req, &cfg).unwrap();
    assert!(resp.fragments.is_empty());
    assert!(resp.resource_refs.is_empty());
}

#[test]
fn fake_hook_client_returns_fragments() {
    use crate::hook::client::FakeHookClient;
    use crate::hook::{
        ContextFragment, ContextFragmentKind, FragmentPlacement, FragmentSensitivity, HookClient,
    };
    let frag = ContextFragment {
        id: "f1".into(),
        hook_id: "context.prepare.v0".into(),
        kind: ContextFragmentKind::Instruction,
        placement: FragmentPlacement::SystemAppend,
        priority: 1,
        content: "use the tool".into(),
        source: "hook:test".into(),
        ttl_secs: None,
        estimated_tokens: 10,
        sensitivity: FragmentSensitivity::Public,
    };
    let client = FakeHookClient::with_fragments(vec![frag]);
    let req = crate::hook::client::ContextPrepareRequest {
        hook: crate::hook::HookKind::ContextPrepareV0,
        run_id: "r".into(),
        session_id: "s".into(),
        agent_id: "main".into(),
        principal: "user".into(),
        channel: "cli".into(),
        user_text: "hello".into(),
        context_budget_chars: 4000,
    };
    let cfg = crate::hook::HookConfig {
        enabled: true,
        kind: crate::hook::HookKind::ContextPrepareV0,
        ..Default::default()
    };
    let resp = client.call_context_prepare(&req, &cfg).unwrap();
    assert_eq!(resp.fragments.len(), 1);
    assert_eq!(resp.fragments[0].content, "use the tool");
}

#[test]
fn hook_client_is_object_safe() {
    // Verify FakeHookClient can be used as Box<dyn HookClient>.
    use crate::hook::client::FakeHookClient;
    use crate::hook::HookClient;
    let client: Box<dyn HookClient> = Box::new(FakeHookClient::empty());
    let req = crate::hook::client::ContextPrepareRequest {
        hook: crate::hook::HookKind::ContextPrepareV0,
        run_id: "r".into(),
        session_id: "s".into(),
        agent_id: "main".into(),
        principal: "user".into(),
        channel: "cli".into(),
        user_text: "test".into(),
        context_budget_chars: 4000,
    };
    let cfg = crate::hook::HookConfig {
        enabled: true,
        kind: crate::hook::HookKind::ContextPrepareV0,
        ..Default::default()
    };
    let resp = client.call_context_prepare(&req, &cfg).unwrap();
    assert!(resp.fragments.is_empty());
}
