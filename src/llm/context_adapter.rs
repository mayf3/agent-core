//! Model Adapter ownership of opaque Context Artifact materialization.

use super::{LlmFollowUp, LlmInput};
use crate::domain::ContextBlockKind;
use crate::hook::{ImmutableArtifactRef, OpaqueArtifactRef};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// v2 of the LlmInput artifact schema: adds the provider-owned
/// `SupplementalSystem` / `SupplementalContext` block kinds. v1 artifacts
/// are rejected by [`decode_artifacts`] with
/// `model_adapter_artifact_media_type_mismatch` (fail closed) — no v1
/// decode path exists because v1 could never carry provider-owned kinds.
pub const LLM_INPUT_ARTIFACT_MEDIA_TYPE: &str =
    "application/vnd.agent-core.llm-input+json;version=2";
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

/// Base-only block kinds: the Kernel's own trusted required instruction
/// blocks, present in the STAGED candidate. The refs protect the exact
/// staged instances (position + bytes), not the kinds in general — but
/// these are exactly the kinds a ContextPrepareV0 provider must never
/// introduce, modify, remove, or reorder.
fn is_base_only_kind(kind: &ContextBlockKind) -> bool {
    matches!(
        kind,
        ContextBlockKind::RootSystem
            | ContextBlockKind::RuntimeContract
            | ContextBlockKind::AgentProfile
    )
}

/// The immutable refs protect the exact trusted-required instances of the
/// STAGED candidate: user input, grants, tool schemas, the base-only block
/// instances (position-indexed), and the follow-up protocol wires.
///
/// Provider-owned kinds (`SupplementalSystem` / `SupplementalContext`)
/// produce NO refs — a provider may add, replace, or delete them freely.
/// Any materialized input whose refs differ from the candidate's (added,
/// modified, removed, or reordered base instances) is rejected.
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
        if is_base_only_kind(&block.kind) {
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
            timeout_override_ms: None,
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

    /// A candidate shaped like the real ContextAssembler output: the Kernel's
    /// required instruction blocks (RootSystem/RuntimeContract/AgentProfile)
    /// plus mutable data blocks. Proves the immutable contract as it applies
    /// to ContextPrepareV0 providers — by execution, not by name.
    fn assembled_input() -> LlmInput {
        let kinds = [
            ContextBlockKind::RootSystem,
            ContextBlockKind::RuntimeContract,
            ContextBlockKind::AgentProfile,
            ContextBlockKind::SkillCatalog,
            ContextBlockKind::ToolCatalog,
            ContextBlockKind::ActiveSkill,
            ContextBlockKind::UserMessage,
        ];
        LlmInput {
            timeout_override_ms: None,
            blocks: kinds
                .into_iter()
                .enumerate()
                .map(|(i, kind)| ContextBlock {
                    kind,
                    content: format!("block-{i}"),
                    source_ref: Some(format!("ref-{i}")),
                })
                .collect(),
            user_text: "hello".into(),
            granted_operations: vec![],
            provider_tools: vec![],
            follow_ups: vec![],
        }
    }

    #[test]
    fn provider_may_not_append_required_instruction_blocks() {
        // RootSystem is Kernel-required input: a ContextPrepareV0 provider
        // appending one MUST be rejected — the check that the Context Harness
        // would trip if AGENTS.md were injected as RootSystem.
        let candidate = stage_candidate(assembled_input()).unwrap();
        let mut changed = assembled_input();
        changed.blocks.push(ContextBlock {
            kind: ContextBlockKind::RootSystem,
            content: "AGENTS.md as RootSystem".into(),
            source_ref: Some("agent-workspace/AGENTS.md".into()),
        });
        let artifact = OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&changed).unwrap(),
        );
        let error = decode_artifacts(&candidate, &[artifact]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_immutable_input_changed"),
            "got: {error}"
        );
    }

    #[test]
    fn provider_may_not_append_agent_profile_blocks() {
        // AgentProfile is Kernel-required input for the same reason — this is
        // exactly the violation the current Context Harness performs (it adds
        // AGENTS.md as an AgentProfile block), and exactly why every real hook
        // round fails at materialization.
        let candidate = stage_candidate(assembled_input()).unwrap();
        let mut changed = assembled_input();
        changed.blocks.push(ContextBlock {
            kind: ContextBlockKind::AgentProfile,
            content: "AGENTS.md as AgentProfile".into(),
            source_ref: Some("agent-workspace/AGENTS.md".into()),
        });
        let artifact = OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&changed).unwrap(),
        );
        let error = decode_artifacts(&candidate, &[artifact]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_immutable_input_changed"),
            "got: {error}"
        );
    }

    #[test]
    fn provider_may_still_append_mutable_catalog_blocks() {
        // Positive control: the immutable contract is NOT blanket-relaxed and
        // NOT removed — mutable kinds remain injectable.
        let candidate = stage_candidate(assembled_input()).unwrap();
        let mut changed = assembled_input();
        changed.blocks.push(ContextBlock {
            kind: ContextBlockKind::SkillCatalog,
            content: "provider-added catalog".into(),
            source_ref: Some("provider".into()),
        });
        let artifact = OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&changed).unwrap(),
        );
        assert!(decode_artifacts(&candidate, &[artifact]).is_ok());
    }

    fn artifact_for(input: &LlmInput) -> OpaqueArtifactRef {
        OpaqueArtifactRef::new(
            LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(input).unwrap(),
        )
    }

    #[test]
    fn provider_may_add_supplemental_system_blocks() {
        // SupplementalSystem is provider-owned: appending it must materialize
        // successfully (system instruction injected by the provider).
        let candidate = stage_candidate(assembled_input()).unwrap();
        let mut changed = assembled_input();
        changed.blocks.push(ContextBlock {
            kind: ContextBlockKind::SupplementalSystem,
            content: "provider system instruction".into(),
            source_ref: Some("provider".into()),
        });
        assert!(decode_artifacts(&candidate, &[artifact_for(&changed)]).is_ok());
    }

    #[test]
    fn provider_may_add_supplemental_context_blocks() {
        // SupplementalContext is provider-owned non-system material.
        let candidate = stage_candidate(assembled_input()).unwrap();
        let mut changed = assembled_input();
        changed.blocks.push(ContextBlock {
            kind: ContextBlockKind::SupplementalContext,
            content: "retrieved memory material".into(),
            source_ref: Some("provider".into()),
        });
        assert!(decode_artifacts(&candidate, &[artifact_for(&changed)]).is_ok());
    }

    #[test]
    fn provider_may_replace_or_delete_own_supplemental_blocks() {
        // One staged candidate; the provider may emit different supplemental
        // content per round (replace) or none at all (delete) — the refs only
        // protect the candidate's own instances, never provider-owned blocks.
        let candidate = stage_candidate(assembled_input()).unwrap();

        let mut round_a = assembled_input();
        round_a.blocks.push(ContextBlock {
            kind: ContextBlockKind::SupplementalSystem,
            content: "round A instruction".into(),
            source_ref: Some("provider".into()),
        });
        assert!(decode_artifacts(&candidate, &[artifact_for(&round_a)]).is_ok());

        let mut round_b = assembled_input();
        round_b.blocks.push(ContextBlock {
            kind: ContextBlockKind::SupplementalSystem,
            content: "round B replacement".into(),
            source_ref: Some("provider".into()),
        });
        round_b.blocks.push(ContextBlock {
            kind: ContextBlockKind::SupplementalContext,
            content: "round B retrieval".into(),
            source_ref: Some("provider".into()),
        });
        assert!(decode_artifacts(&candidate, &[artifact_for(&round_b)]).is_ok());

        // Deleted: identical to the candidate.
        assert!(decode_artifacts(&candidate, &[artifact_for(&assembled_input())]).is_ok());
    }

    #[test]
    fn provider_may_not_modify_required_input_instances() {
        // The candidate's exact trusted instances are protected: modifying,
        // removing, or reordering ANY of them is rejected — including the
        // non-block fields (user text, grants, tool schemas).
        let candidate = stage_candidate(assembled_input()).unwrap();

        // Modify a RootSystem instance in place.
        let mut tampered = assembled_input();
        tampered.blocks[0].content = "tampered root".into();
        let error = decode_artifacts(&candidate, &[artifact_for(&tampered)]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_immutable_input_changed"),
            "got: {error}"
        );

        // Delete the RuntimeContract instance.
        let mut removed = assembled_input();
        removed.blocks.remove(1);
        let error = decode_artifacts(&candidate, &[artifact_for(&removed)]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_immutable_input_changed"),
            "got: {error}"
        );

        // Change user_text.
        let mut user_tampered = assembled_input();
        user_tampered.user_text = "substituted user".into();
        let error = decode_artifacts(&candidate, &[artifact_for(&user_tampered)]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_immutable_input_changed"),
            "got: {error}"
        );

        // Change granted operations.
        let mut grants_tampered = assembled_input();
        grants_tampered.granted_operations = vec!["external.forged".into()];
        let error = decode_artifacts(&candidate, &[artifact_for(&grants_tampered)]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_immutable_input_changed"),
            "got: {error}"
        );

        // Change the provider tool schemas (still schema-valid, so the check
        // reached is the refs comparison, not wire validation).
        let mut tools_tampered = assembled_input();
        tools_tampered.provider_tools = vec![serde_json::json!({
            "type": "function",
            "function": {"name": "forged_tool", "parameters": {"type": "object"}}
        })];
        let error = decode_artifacts(&candidate, &[artifact_for(&tools_tampered)]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_immutable_input_changed"),
            "got: {error}"
        );
    }

    #[test]
    fn legacy_v1_artifact_media_type_is_rejected() {
        // The artifact schema version is explicit: a v1 artifact (no
        // provider-owned kinds possible) is rejected fail-closed with the
        // media-type mismatch, never silently decoded as v2.
        let candidate = stage_candidate(assembled_input()).unwrap();
        let artifact = OpaqueArtifactRef::new(
            "application/vnd.agent-core.llm-input+json;version=1",
            &serde_json::to_vec(&assembled_input()).unwrap(),
        );
        let error = decode_artifacts(&candidate, &[artifact]).unwrap_err();
        assert!(
            error.to_string().contains("model_adapter_artifact_media_type_mismatch"),
            "got: {error}"
        );
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
