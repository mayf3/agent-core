//! Model Adapter ownership of opaque Context Artifact materialization.

use super::{LlmFollowUp, LlmInput};
use crate::domain::ContextBlockKind;
use crate::hook::{ImmutableArtifactRef, OpaqueArtifactRef};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LLM_INPUT_ARTIFACT_MEDIA_TYPE: &str =
    "application/vnd.agent-core.llm-input+json;version=1";
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 128_000;
pub const DEFAULT_RESERVED_OUTPUT_TOKENS: usize = 4_096;

#[derive(Debug, Clone)]
pub struct AdapterCandidate {
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub immutable_refs: Vec<ImmutableArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardBudgetReceipt {
    pub input_tokens: usize,
    pub reserved_output_tokens: usize,
    pub context_window_tokens: usize,
}

impl HardBudgetReceipt {
    pub fn within_budget(&self) -> bool {
        self.input_tokens
            .checked_add(self.reserved_output_tokens)
            .is_some_and(|total| total <= self.context_window_tokens)
    }
}

#[derive(Debug)]
pub enum ModelMaterialization {
    Ready {
        input: LlmInput,
        budget: HardBudgetReceipt,
    },
    OverBudget {
        budget: HardBudgetReceipt,
    },
}

pub fn stage_candidate(input: LlmInput) -> Result<AdapterCandidate> {
    validate_wire_input(&input)?;
    let immutable_refs = immutable_refs_for(&input)?;
    Ok(AdapterCandidate {
        media_type: LLM_INPUT_ARTIFACT_MEDIA_TYPE.into(),
        bytes: serde_json::to_vec(&input)?,
        immutable_refs,
    })
}

pub fn decode_artifacts(
    candidate: &AdapterCandidate,
    artifacts: &[OpaqueArtifactRef],
) -> Result<LlmInput> {
    if artifacts.is_empty() {
        bail!("model_adapter_artifacts_empty");
    }
    let mut bytes = Vec::new();
    for artifact in artifacts {
        if artifact.media_type != candidate.media_type {
            bail!("model_adapter_artifact_media_type_mismatch");
        }
        bytes.extend(artifact.decode_verified()?);
    }
    let input: LlmInput =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("model_adapter_artifact"))?;
    validate_wire_input(&input)?;
    if immutable_refs_for(&input)? != candidate.immutable_refs {
        bail!("model_adapter_immutable_input_changed");
    }
    Ok(input)
}

pub fn assess_budget(
    input: LlmInput,
    materialized_wires: &[Value],
    context_window_tokens: usize,
    reserved_output_tokens: usize,
) -> Result<ModelMaterialization> {
    let tokenizer =
        tiktoken_rs::cl100k_base().map_err(|_| anyhow::anyhow!("model_tokenizer_unavailable"))?;
    let mut input_tokens = 0;
    for materialized_wire in materialized_wires {
        let wire = serde_json::to_string(materialized_wire)?;
        input_tokens = input_tokens.max(tokenizer.encode_ordinary(&wire).len());
    }
    let receipt = HardBudgetReceipt {
        input_tokens,
        reserved_output_tokens,
        context_window_tokens,
    };
    if receipt.within_budget() {
        Ok(ModelMaterialization::Ready {
            input,
            budget: receipt,
        })
    } else {
        Ok(ModelMaterialization::OverBudget { budget: receipt })
    }
}

pub fn default_materialize(
    candidate: &AdapterCandidate,
    artifacts: &[OpaqueArtifactRef],
) -> Result<ModelMaterialization> {
    let input = decode_artifacts(candidate, artifacts)?;
    let wire = serde_json::to_value(&input)?;
    assess_budget(
        input,
        &[wire],
        DEFAULT_CONTEXT_WINDOW_TOKENS,
        DEFAULT_RESERVED_OUTPUT_TOKENS,
    )
}

fn immutable_refs_for(input: &LlmInput) -> Result<Vec<ImmutableArtifactRef>> {
    let mut refs = vec![
        ImmutableArtifactRef::new("model:user-input", input.user_text.as_bytes()),
        ImmutableArtifactRef::new(
            "model:granted-operations",
            &serde_json::to_vec(&input.granted_operations)?,
        ),
        ImmutableArtifactRef::new(
            "model:tool-schema",
            &serde_json::to_vec(&input.provider_tools)?,
        ),
    ];
    for (index, block) in input.blocks.iter().enumerate() {
        if matches!(
            block.kind,
            ContextBlockKind::RootSystem
                | ContextBlockKind::RuntimeContract
                | ContextBlockKind::AgentProfile
        ) {
            refs.push(ImmutableArtifactRef::new(
                format!("model:required-block:{index}"),
                &serde_json::to_vec(block)?,
            ));
        }
    }
    for (index, follow_up) in input.follow_ups.iter().enumerate() {
        refs.push(ImmutableArtifactRef::new(
            format!("model:follow-up-wire:{index}"),
            &serde_json::to_vec(&follow_up.provider_turn)?,
        ));
    }
    Ok(refs)
}

fn validate_wire_input(input: &LlmInput) -> Result<()> {
    for tool in &input.provider_tools {
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            bail!("model_adapter_tool_type_invalid");
        }
        let name = tool.pointer("/function/name").and_then(Value::as_str);
        if name.is_none_or(|name| name.trim().is_empty()) {
            bail!("model_adapter_tool_name_invalid");
        }
        if !tool
            .pointer("/function/parameters")
            .is_some_and(Value::is_object)
        {
            bail!("model_adapter_tool_schema_invalid");
        }
    }
    for follow_up in &input.follow_ups {
        validate_follow_up(follow_up)?;
    }
    Ok(())
}

fn validate_follow_up(follow_up: &LlmFollowUp) -> Result<()> {
    let turn = &follow_up.provider_turn;
    if turn.provider_tool_call_id.trim().is_empty()
        || turn.wire_name.trim().is_empty()
        || turn.canonical_operation.trim().is_empty()
    {
        bail!("model_adapter_tool_pair_required_field_empty");
    }
    let arguments: Value = serde_json::from_str(&turn.arguments_json)
        .map_err(|_| anyhow::anyhow!("model_adapter_tool_arguments_invalid"))?;
    if !arguments.is_object() {
        bail!("model_adapter_tool_arguments_not_object");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ContextBlock, ContextBlockKind};
    use crate::llm::{EndpointChoice, ProviderToolTurn};

    fn input() -> LlmInput {
        LlmInput {
            blocks: vec![ContextBlock {
                kind: ContextBlockKind::UserMessage,
                content: "original".into(),
                source_ref: Some("input".into()),
            }],
            user_text: "hello".into(),
            granted_operations: vec![],
            provider_tools: vec![],
            follow_ups: vec![],
        }
    }

    #[test]
    fn provider_may_change_context_but_not_immutable_model_fields() {
        let candidate = stage_candidate(input()).unwrap();
        let mut changed = input();
        changed.blocks[0].content = "provider changed".into();
        let artifact = OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&changed).unwrap(),
        );
        assert!(decode_artifacts(&candidate, &[artifact]).is_ok());

        changed.user_text = "changed user".into();
        let artifact = OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&changed).unwrap(),
        );
        assert!(decode_artifacts(&candidate, &[artifact]).is_err());
    }

    #[test]
    fn ordered_artifact_bytes_are_materialized() {
        let candidate = stage_candidate(input()).unwrap();
        let bytes = serde_json::to_vec(&input()).unwrap();
        let split = bytes.len() / 2;
        let refs = vec![
            OpaqueArtifactRef::new(LLM_INPUT_ARTIFACT_MEDIA_TYPE, &bytes[..split]),
            OpaqueArtifactRef::new(LLM_INPUT_ARTIFACT_MEDIA_TYPE, &bytes[split..]),
        ];
        assert!(decode_artifacts(&candidate, &refs).is_ok());
    }

    #[test]
    fn provider_may_change_tool_result_content_but_not_follow_up_wire() {
        let mut original = input();
        original.follow_ups.push(LlmFollowUp {
            provider_turn: ProviderToolTurn {
                endpoint: EndpointChoice::Primary,
                provider_tool_call_id: "call-1".into(),
                wire_name: "system_status".into(),
                canonical_operation: "system.status".into(),
                arguments_json: "{}".into(),
                reasoning_content: None,
            },
            result_content: "full result".into(),
        });
        let candidate = stage_candidate(original.clone()).unwrap();

        let mut changed_result = original.clone();
        changed_result.follow_ups[0].result_content = "provider summary".into();
        let artifact = OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&changed_result).unwrap(),
        );
        assert!(decode_artifacts(&candidate, &[artifact]).is_ok());

        let mut changed_wire = original;
        changed_wire.follow_ups[0]
            .provider_turn
            .provider_tool_call_id = "substituted-call".into();
        let artifact = OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&changed_wire).unwrap(),
        );
        assert!(decode_artifacts(&candidate, &[artifact]).is_err());
    }
}
