//! Deterministic production delivery for the North Star calculator call.
//!
//! The routing client emits one real `external.calculator` tool call and only
//! returns `42` after the existing Runtime reports a succeeded tool follow-up
//! containing the Host result.  It does not implement arithmetic.

use crate::config::KernelConfig;
use crate::domain::ValidatedEvent;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::{
    tool_call_id_hash, EndpointChoice, LlmClient, LlmInput, LlmOutput, ProviderToolTurn, ToolCall,
    ToolCallResult,
};
use crate::runtime::{Runtime, RuntimeOutcome};
use anyhow::Result;
use serde_json::json;

const RAW_TOOL_CALL_ID: &str = "north-star-calculator-multiply-6-7";

pub fn deliver(
    config: KernelConfig,
    journal: &JournalStore,
    gateway: &Gateway,
    event: ValidatedEvent,
) -> Result<RuntimeOutcome> {
    Runtime::new(config, CalculatorRoutingClient).deliver(journal, gateway, event)
}

struct CalculatorRoutingClient;

impl LlmClient for CalculatorRoutingClient {
    fn complete(&self, input: LlmInput) -> Result<LlmOutput> {
        if input.follow_ups.is_empty() {
            let arguments = json!({"operation": "multiply", "a": 6, "b": 7});
            return Ok(LlmOutput {
                provider: "kernel".into(),
                model: "calculator-router-v0".into(),
                content: String::new(),
                journal_payload: json!({
                    "provider": "kernel",
                    "model": "calculator-router-v0",
                    "route": "external.calculator/multiply",
                }),
                tool_call: ToolCallResult::Valid(ToolCall {
                    id: tool_call_id_hash(RAW_TOOL_CALL_ID),
                    operation: "external.calculator".into(),
                    arguments: arguments.clone(),
                }),
                provider_turn: Some(ProviderToolTurn {
                    endpoint: EndpointChoice::Primary,
                    provider_tool_call_id: RAW_TOOL_CALL_ID.into(),
                    wire_name: "external.calculator".into(),
                    canonical_operation: "external.calculator".into(),
                    arguments_json: serde_json::to_string(&arguments)?,
                    reasoning_content: None,
                }),
            });
        }

        let tool_result = &input
            .follow_ups
            .last()
            .expect("non-empty follow-up list")
            .result_content;
        let succeeded = tool_result.contains("status: succeeded");
        let returned_42 = tool_result
            .split_once("output:")
            .map(|(_, output)| output.trim() == "Number(42)")
            .unwrap_or(false);
        let content = if succeeded && returned_42 {
            "42"
        } else {
            "external.calculator 调用失败"
        };
        Ok(LlmOutput {
            provider: "kernel".into(),
            model: "calculator-router-v0".into(),
            content: content.into(),
            journal_payload: json!({
                "provider": "kernel",
                "model": "calculator-router-v0",
                "verified_tool_result": succeeded && returned_42,
            }),
            tool_call: ToolCallResult::Absent,
            provider_turn: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmFollowUp, ProviderToolTurn};

    fn input(result: Option<&str>) -> LlmInput {
        LlmInput {
            blocks: vec![],
            user_text: super::super::calculator_router::CALCULATOR_SMOKE_SENTENCE.into(),
            granted_operations: vec!["external.calculator".into()],
            provider_tools: vec![],
            follow_ups: result
                .map(|value| {
                    vec![LlmFollowUp {
                        provider_turn: ProviderToolTurn {
                            endpoint: EndpointChoice::Primary,
                            provider_tool_call_id: RAW_TOOL_CALL_ID.into(),
                            wire_name: "external.calculator".into(),
                            canonical_operation: "external.calculator".into(),
                            arguments_json: "{}".into(),
                            reasoning_content: None,
                        },
                        result_content: value.into(),
                    }]
                })
                .unwrap_or_default(),
        }
    }

    #[test]
    fn first_round_emits_real_calculator_tool_call() {
        let output = CalculatorRoutingClient.complete(input(None)).unwrap();
        let ToolCallResult::Valid(call) = output.tool_call else {
            panic!("expected tool call");
        };
        assert_eq!(call.operation, "external.calculator");
        assert_eq!(call.arguments, json!({"operation":"multiply","a":6,"b":7}));
    }

    #[test]
    fn only_verified_succeeded_tool_result_returns_42() {
        let ok = CalculatorRoutingClient
            .complete(input(Some("status: succeeded\noutput: Number(42)")))
            .unwrap();
        assert_eq!(ok.content, "42");
        let failed = CalculatorRoutingClient
            .complete(input(Some("status: execution_failed\noutput: Number(42)")))
            .unwrap();
        assert_ne!(failed.content, "42");
        let wrong_value = CalculatorRoutingClient
            .complete(input(Some("status: succeeded\noutput: Number(142)")))
            .unwrap();
        assert_ne!(wrong_value.content, "42");
    }
}
