use serde_json::json;

/// Minimal OperationSpec for testing schema validation.
fn spec(name: &str, params: serde_json::Value) -> crate::registry::snapshot::OperationSpec {
    use crate::registry::snapshot::{BindingKind, Risk};
    crate::registry::snapshot::OperationSpec {
        name: name.into(),
        risk: Risk::ReadOnly,
        description: "test".into(),
        parameters: params,
        idempotent: true,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.test".into(),
    }
}

#[test]
fn validate_model_arguments_returns_typed_rejections() {
    use crate::gateway::ToolRejection;
    use crate::runtime::validate_model_arguments;

    // Known typed operations keep their existing validation.
    assert_eq!(
        validate_model_arguments(
            "system.status",
            &json!({"x": 1}),
            &spec(
                "system.status",
                json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false})
            ),
        ),
        Err(ToolRejection::InvalidArguments)
    );
    assert_eq!(
        validate_model_arguments(
            "session.recall_recent",
            &json!({"limit": 0}),
            &spec(
                "session.recall_recent",
                json!({"type": "object", "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 20}}}),
            )
        ),
        Err(ToolRejection::InvalidArguments)
    );
    assert_eq!(
        validate_model_arguments(
            "time.now",
            &json!("nope"),
            &spec("time.now", json!({"type": "object"})),
        ),
        Err(ToolRejection::MalformedArguments)
    );

    // Unknown operations in the `_` arm are validated against the spec schema.
    // Empty params with no constraints → valid.
    assert!(validate_model_arguments(
        "harness.op",
        &json!({}),
        &spec("harness.op", json!({"type": "object"})),
    )
    .is_ok());

    // Unknown operation with unknown key and additionalProperties: false → rejected.
    assert_eq!(
        validate_model_arguments(
            "harness.op",
            &json!({"unknown_key": "value"}),
            &spec(
                "harness.op",
                json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false})
            ),
        ),
        Err(ToolRejection::InvalidArguments)
    );

    // Known typed operations still pass with valid args.
    assert!(validate_model_arguments(
        "time.now",
        &json!({}),
        &spec("time.now", json!({"type": "object"})),
    )
    .is_ok());
}

#[test]
fn typed_rejection_categories_and_messages_are_safe() {
    use crate::gateway::ToolRejection;
    for r in [
        ToolRejection::UnknownOperation,
        ToolRejection::OperationNotAllowed,
        ToolRejection::MalformedArguments,
        ToolRejection::InvalidArguments,
        ToolRejection::PolicyDenied,
        ToolRejection::MalformedToolCall,
        ToolRejection::InternalToolError,
    ] {
        let (cat, msg) = (r.category(), r.safe_message());
        assert!(!cat.is_empty() && cat.len() <= 32);
        assert!(!msg.is_empty() && msg.len() <= 80);
        assert!(cat
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()));
    }
}

#[test]
fn tool_call_result_absent_is_absent() {
    use crate::llm::ToolCallResult;
    assert!(ToolCallResult::Absent.is_absent());
}
