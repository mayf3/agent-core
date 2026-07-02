use crate::registry::snapshot::{BindingKind, OperationSpec, Risk};
use serde_json::json;

fn builtin_spec(name: &str, risk: Risk) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        risk,
        description: "test".into(),
        parameters: json!({"type": "object"}),
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key: format!("builtin.{name}"),
    }
}

#[test]
fn validate_model_arguments_returns_typed_rejections() {
    use crate::gateway::ToolRejection;
    use crate::runtime::validate_model_arguments;
    let spec_sys = builtin_spec("system.status", Risk::ReadOnly);
    let spec_session = builtin_spec("session.recall_recent", Risk::ReadOnly);
    let spec_time = builtin_spec("system.status", Risk::ReadOnly);
    assert_eq!(
        validate_model_arguments(&spec_sys, &json!({"x": 1})),
        Err(ToolRejection::InvalidArguments)
    );
    assert_eq!(
        validate_model_arguments(&spec_session, &json!({"limit": 0})),
        Err(ToolRejection::InvalidArguments)
    );
    assert_eq!(
        validate_model_arguments(&spec_time, &json!("nope")),
        Err(ToolRejection::MalformedArguments)
    );
    let spec_unknown = OperationSpec {
        name: "shell.exec".into(),
        risk: Risk::Write,
        description: "test".into(),
        parameters: json!({"type": "object"}),
        idempotent: false,
        binding_kind: BindingKind::Builtin,
        binding_key: "builtin.shell_exec".into(),
    };
    assert_eq!(
        validate_model_arguments(&spec_unknown, &json!({})),
        Err(ToolRejection::UnknownOperation)
    );
    assert!(validate_model_arguments(&spec_time, &json!({})).is_ok());
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
