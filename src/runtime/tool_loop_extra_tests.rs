use serde_json::json;

#[test]
fn validate_model_arguments_returns_typed_rejections() {
    use crate::gateway::ToolRejection;
    use crate::runtime::validate_model_arguments;
    assert_eq!(
        validate_model_arguments("system.status", &json!({"x": 1})),
        Err(ToolRejection::InvalidArguments)
    );
    assert_eq!(
        validate_model_arguments("session.recall_recent", &json!({"limit": 0})),
        Err(ToolRejection::InvalidArguments)
    );
    assert_eq!(
        validate_model_arguments("time.now", &json!("nope")),
        Err(ToolRejection::MalformedArguments)
    );
    assert_eq!(
        validate_model_arguments("shell.exec", &json!({})),
        Err(ToolRejection::UnknownOperation)
    );
    assert!(validate_model_arguments("time.now", &json!({})).is_ok());
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
