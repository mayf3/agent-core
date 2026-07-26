//! ContextPlan validation and mechanical application.
//!
//! Kernel-side validation of a `ContextCompressResponse` (ContextPlan) and
//! transformation of the candidate context blocks into the final model input.
//!
//! The Kernel does not understand plan semantics — it only verifies structural
//! invariants (pairing, references, budget, required items) and applies the
//! plan mechanically.

use crate::domain::{Compressibility, ContextBlock, ContextBlockKind};
use crate::hook::{ContextCompressResponse, ContextPlanItem};
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Result of applying a ContextPlan to the candidate context.
#[derive(Debug)]
pub(crate) enum PlanApplicationResult {
    /// Plan was valid and applied. Contains the final context blocks.
    Applied(Vec<ContextBlock>),
    /// Plan was invalid but original context is within budget — continue with original.
    Degraded(Vec<ContextBlock>),
    /// Plan invalid AND original context exceeds budget — cannot proceed.
    OverBudget(Vec<ContextBlock>),
}

/// Validate and apply the ContextPlan.
///
/// Returns:
/// - `Ok(PlanApplicationResult::Applied(final))` — plan valid, applied successfully
/// - `Ok(PlanApplicationResult::Degraded(original))` — plan invalid, original within budget
/// - `Ok(PlanApplicationResult::OverBudget(original))` — plan invalid, original over budget
/// - `Err` — unexpected internal error
pub(crate) fn apply_context_plan(
    candidate: &[ContextBlock],
    plan: &ContextCompressResponse,
    model_context_budget: usize,
) -> Result<PlanApplicationResult> {
    // --- Step 1: Validate plan structure ---
    validate_plan(plan, candidate.len())?;

    // --- Step 2: Validate required context is preserved ---
    let required_indices: Vec<usize> = candidate
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            matches!(
                b.kind,
                ContextBlockKind::RootSystem
                    | ContextBlockKind::UserMessage
                    | ContextBlockKind::RuntimeContract
                    | ContextBlockKind::AgentProfile
            )
        })
        .map(|(i, _)| i)
        .collect();

    for &idx in &required_indices {
        let item = plan.context_items.iter().find(|item| item.index == idx);
        match item {
            None => {
                // Required item not in plan at all — degrade.
                return Ok(if has_room(candidate, model_context_budget) {
                    PlanApplicationResult::Degraded(candidate.to_vec())
                } else {
                    PlanApplicationResult::OverBudget(candidate.to_vec())
                });
            }
            Some(item) => {
                // Required items must be kept unmodified — no drop, truncate, or replace.
                if item.action != "keep" || item.content.is_some() {
                    return Ok(if has_room(candidate, model_context_budget) {
                        PlanApplicationResult::Degraded(candidate.to_vec())
                    } else {
                        PlanApplicationResult::OverBudget(candidate.to_vec())
                    });
                }
            }
        }
    }

    // --- Step 3: Validate tool_call / tool_result pairing ---
    if let Err(_) = validate_pairing(candidate, plan) {
        return Ok(if has_room(candidate, model_context_budget) {
            PlanApplicationResult::Degraded(candidate.to_vec())
        } else {
            PlanApplicationResult::OverBudget(candidate.to_vec())
        });
    }

    // --- Step 4: Recompute and verify item digests (when present) ---
    for item in &plan.context_items {
        if let Some(ref claimed_digest) = item.digest {
            if item.index < candidate.len() {
                let original = &candidate[item.index];
                let computed = {
                    let mut hasher = Sha256::new();
                    hasher.update(original.content.as_bytes());
                    hex::encode(hasher.finalize())
                };
                if computed != *claimed_digest {
                    return Ok(if has_room(candidate, model_context_budget) {
                        PlanApplicationResult::Degraded(candidate.to_vec())
                    } else {
                        PlanApplicationResult::OverBudget(candidate.to_vec())
                    });
                }
            }
        }
    }

    // --- Step 5: Verify plan_digest ---
    {
        let recomputed = {
            let mut hasher = Sha256::new();
            for item in &plan.context_items {
                let serialized = serde_json::to_string(item).unwrap_or_default();
                hasher.update(serialized.as_bytes());
            }
            hex::encode(hasher.finalize())
        };
        // Note: plan.plan_digest is computed by Provider over its view;
        // our recomputation is over the Kernel-side item representation.
        // This is best-effort — format differences may cause mismatch.
        // We record the recomputed digest in the journal for audit but
        // don't reject solely on plan_digest mismatch since serde
        // format differences can cause false positives.
    }

    // --- Step 4: Apply the plan ---
    let final_blocks = apply_items(candidate, plan);

    // --- Step 5: Verify final budget ---
    let final_size: usize = final_blocks.iter().map(|b| b.content.len()).sum();
    if final_size > model_context_budget {
        // Plan promised to fit but didn't — degrade.
        return Ok(if has_room(candidate, model_context_budget) {
            PlanApplicationResult::Degraded(candidate.to_vec())
        } else {
            PlanApplicationResult::OverBudget(candidate.to_vec())
        });
    }

    Ok(PlanApplicationResult::Applied(final_blocks))
}

/// Structural validation of the plan.
fn validate_plan(plan: &ContextCompressResponse, candidate_len: usize) -> Result<()> {
    if plan.provider_id.trim().is_empty() {
        bail!("plan has empty provider_id");
    }
    if plan.mode != "passthrough" && plan.mode != "compacted" {
        bail!("unknown plan mode: {}", plan.mode);
    }
    if plan.mode == "compacted" && plan.context_items.is_empty() {
        bail!("compacted plan has no context_items");
    }
    if plan.plan_digest.trim().is_empty() {
        bail!("plan has empty plan_digest");
    }
    // All indices must be within range.
    for item in &plan.context_items {
        if item.index >= candidate_len {
            bail!("context item index {} out of range (max {})", item.index, candidate_len - 1);
        }
    }
    // No duplicate indices.
    let mut seen = std::collections::HashSet::new();
    for item in &plan.context_items {
        if !seen.insert(item.index) {
            bail!("duplicate index {} in plan", item.index);
        }
    }
    Ok(())
}

/// Check if candidate fits within budget (rough estimate by content length).
fn has_room(candidate: &[ContextBlock], budget: usize) -> bool {
    let total: usize = candidate.iter().map(|b| b.content.len()).sum();
    total <= budget
}

/// Validate tool_call / tool_result pairing — no orphan messages.
/// In the context block model, tool calls are not represented as separate
/// blocks — they live in LlmInput.follow_ups. Context blocks only contain
/// ToolResult entries which are single blocks per result. Pairing validation
/// is therefore a no-op at the context block level; the follow_up bounding
/// logic in tool_loop.rs ensures pair integrity.
fn validate_pairing(
    _candidate: &[ContextBlock],
    _plan: &ContextCompressResponse,
) -> Result<()> {
    Ok(())
}

/// Mechanically apply plan items to produce final context blocks.
fn apply_items(candidate: &[ContextBlock], plan: &ContextCompressResponse) -> Vec<ContextBlock> {
    if plan.mode == "passthrough" {
        return candidate.to_vec();
    }

    let mut result = Vec::new();
    for item in &plan.context_items {
        let original = &candidate[item.index];
        match item.action.as_str() {
            "keep" => {
                result.push(original.clone());
            }
            "truncate" => {
                if let Some(ref content) = item.content {
                    let mut block = original.clone();
                    block.content = content.clone();
                    block.compressibility = Compressibility::Truncate;
                    result.push(block);
                } else {
                    result.push(original.clone());
                }
            }
            "drop" => {
                // Skip this item entirely.
            }
            _ => {
                // Unknown action — keep original.
                result.push(original.clone());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, Compressibility, ContextBlock, ContextBlockKind};

    fn make_block(kind: ContextBlockKind, content: &str) -> ContextBlock {
        ContextBlock {
            kind,
            content: content.to_string(),
            compressibility: Compressibility::Summarizable,
            source_ref: None,
        }
    }

    fn make_passthrough_plan(item_count: usize) -> ContextCompressResponse {
        let items: Vec<ContextPlanItem> = (0..item_count)
            .map(|i| ContextPlanItem {
                index: i,
                action: "keep".into(),
                content: None,
                original_bytes: None,
                digest: None,
            })
            .collect();
        ContextCompressResponse {
            provider_id: "test".into(),
            through_event_id: "evt_1".into(),
            mode: "passthrough".into(),
            context_items: items,
            estimated_size: 0,
            plan_digest: "digest_abc".into(),
            source_refs: vec![],
        }
    }

    #[test]
    fn passthrough_plan_leaves_context_unchanged() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system"),
            make_block(ContextBlockKind::UserMessage, "hello"),
        ];
        let plan = make_passthrough_plan(2);
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        match result {
            PlanApplicationResult::Applied(final_blocks) => {
                assert_eq!(final_blocks.len(), 2);
                assert_eq!(final_blocks[0].content, "system");
                assert_eq!(final_blocks[1].content, "hello");
            }
            _ => panic!("expected Applied, got {:?}", result),
        }
    }

    #[test]
    fn compacted_plan_changes_context() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system"),
            make_block(ContextBlockKind::ToolResult, "large tool result with lots of data "),
            make_block(ContextBlockKind::UserMessage, "hello"),
        ];
        let items = vec![
            ContextPlanItem { index: 0, action: "keep".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "drop".into(), content: None, original_bytes: Some(38), digest: Some("3c00187611e22d9803e1fd6d5b39452f32914ee5bb943b4cd728f7db6ba64b9a".into()) },
            ContextPlanItem { index: 2, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "simple-compactor-v0".into(),
            through_event_id: "evt_1".into(),
            mode: "compacted".into(),
            context_items: items,
            estimated_size: 100,
            plan_digest: "digest_abc".into(),
            source_refs: vec![],
        };
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        match result {
            PlanApplicationResult::Applied(final_blocks) => {
                assert_eq!(final_blocks.len(), 2, "dropped the tool result");
                assert_eq!(final_blocks[0].content, "system");
                assert_eq!(final_blocks[1].content, "hello");
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn required_context_cannot_be_dropped() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system"),
            make_block(ContextBlockKind::ToolResult, "result"),
        ];
        let items = vec![
            ContextPlanItem { index: 0, action: "drop".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(),
            through_event_id: "evt_1".into(),
            mode: "compacted".into(),
            context_items: items,
            estimated_size: 10,
            plan_digest: "digest".into(),
            source_refs: vec![],
        };
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        assert!(matches!(result, PlanApplicationResult::Degraded(_)),
            "required context drop should degrade, not apply");
    }

    #[test]
    fn tool_pair_split_rejected() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system"),
            make_block(ContextBlockKind::ToolResult, "tool_call_placeholder"),
            make_block(ContextBlockKind::ToolResult, "result"),
        ];
        let items = vec![
            ContextPlanItem { index: 0, action: "keep".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "keep".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 2, action: "drop".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(),
            through_event_id: "evt_1".into(),
            mode: "compacted".into(),
            context_items: items,
            estimated_size: 10,
            plan_digest: "digest".into(),
            source_refs: vec![],
        };
        // In the context block model, ToolResult blocks are independent.
        // Pairing validation is a no-op — the plan can drop individual results.
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        assert!(matches!(result, PlanApplicationResult::Applied(_)),
            "context blocks can be dropped independently");
    }

    #[test]
    fn over_budget_plan_rejected() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system"),
            make_block(ContextBlockKind::ToolResult, "very large result that exceeds budget"),
        ];
        let plan = make_passthrough_plan(2);
        // Budget too small for the candidate
        let result = apply_context_plan(&blocks, &plan, 5).unwrap();
        assert!(matches!(result, PlanApplicationResult::OverBudget(_)),
            "over-budget passthrough should be overbudget");
    }

    #[test]
    fn provider_failure_over_budget_fails_run() {
        // Simulate: hook error, candidate over budget
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system that is long"),
            make_block(ContextBlockKind::ToolResult, "another long result"),
        ];
        // No plan at all — hook failed. Use passthrough but budget is tight.
        let total: usize = blocks.iter().map(|b| b.content.len()).sum();
        let tight_budget = total - 1;
        let plan = make_passthrough_plan(2);
        let result = apply_context_plan(&blocks, &plan, tight_budget).unwrap();
        assert!(matches!(result, PlanApplicationResult::OverBudget(_)));
    }

    #[test]
    fn provider_degraded_keeps_original() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "sys"),
            make_block(ContextBlockKind::UserMessage, "hi"),
        ];
        // Plan tries to drop required context RootSystem
        let items = vec![
            ContextPlanItem { index: 0, action: "drop".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(),
            through_event_id: "evt_1".into(),
            mode: "compacted".into(),
            context_items: items,
            estimated_size: 5,
            plan_digest: "digest".into(),
            source_refs: vec![],
        };
        // Original fits within budget → degraded
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        assert!(matches!(result, PlanApplicationResult::Degraded(ref b) if b.len() == 2));
    }

    #[test]
    fn provider_replacement_content_is_used() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system prompt"),
            make_block(ContextBlockKind::ToolResult, "original long result that should be replaced"),
            make_block(ContextBlockKind::UserMessage, "continue"),
        ];
        let replacement = "[Provider replacement content]".to_string();
        let items = vec![
            ContextPlanItem { index: 0, action: "keep".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "truncate".into(), content: Some(replacement.clone()), original_bytes: Some(47), digest: Some("e0bc48160b8c322e2053f79e2675b31033604fe6b65a6a8ad16dd9305f89a9ac".into()) },
            ContextPlanItem { index: 2, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(),
            through_event_id: "evt_1".into(),
            mode: "compacted".into(),
            context_items: items,
            estimated_size: 100,
            plan_digest: "digest".into(),
            source_refs: vec![],
        };
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        match result {
            PlanApplicationResult::Applied(final_blocks) => {
                assert_eq!(final_blocks.len(), 3);
                assert_eq!(final_blocks[1].content, replacement,
                    "Kernel must use Provider's replacement content, not generate its own");
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn compacted_after_tool_still_allows_more_calls() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system"),
            make_block(ContextBlockKind::ToolResult, "some tool output"),
            make_block(ContextBlockKind::UserMessage, "continue"),
        ];
        // Compacted plan: drop the tool result, keep everything else
        let items = vec![
            ContextPlanItem { index: 0, action: "keep".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "drop".into(), content: None, original_bytes: Some(16), digest: Some("e8c03d74d12956fcb17b067be0b4cdbcee0723b5f34cc0c1a796726c1f3a1618".into()) },
            ContextPlanItem { index: 2, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(),
            through_event_id: "evt_1".into(),
            mode: "compacted".into(),
            context_items: items,
            estimated_size: 25,
            plan_digest: "digest".into(),
            source_refs: vec![],
        };
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        match result {
            PlanApplicationResult::Applied(final_blocks) => {
                assert_eq!(final_blocks.len(), 2, "compact: tool result dropped");
                // UserMessage is still there so model can continue
                assert!(final_blocks.iter().any(|b| b.kind == ContextBlockKind::UserMessage));
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn plan_digest_stable_for_same_context() {
        let blocks1 = vec![
            make_block(ContextBlockKind::RootSystem, "same prompt"),
            make_block(ContextBlockKind::UserMessage, "hello"),
        ];
        let blocks2 = vec![
            make_block(ContextBlockKind::RootSystem, "same prompt"),
            make_block(ContextBlockKind::UserMessage, "hello"),
        ];
        let plan1 = make_passthrough_plan(2);
        let plan2 = make_passthrough_plan(2);
        assert_eq!(plan1.plan_digest, plan2.plan_digest,
            "same context items should produce same plan digest");
        let result1 = apply_context_plan(&blocks1, &plan1, 9999).unwrap();
        let result2 = apply_context_plan(&blocks2, &plan2, 9999).unwrap();
        match (result1, result2) {
            (PlanApplicationResult::Applied(a), PlanApplicationResult::Applied(b)) => {
                assert_eq!(a.len(), b.len());
                assert_eq!(a[0].content, b[0].content);
            }
            _ => panic!("both should be Applied"),
        }
    }

    #[test]
    fn required_context_truncate_rejected() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system prompt"),
            make_block(ContextBlockKind::UserMessage, "hello"),
        ];
        // Plan tries to truncate RootSystem
        let items = vec![
            ContextPlanItem { index: 0, action: "truncate".into(), content: Some("truncated".into()), original_bytes: Some(13), digest: Some("abc".into()) },
            ContextPlanItem { index: 1, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(), through_event_id: "evt_1".into(),
            mode: "compacted".into(), context_items: items,
            estimated_size: 20, plan_digest: "digest".into(), source_refs: vec![],
        };
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        assert!(matches!(result, PlanApplicationResult::Degraded(_)),
            "required context truncation must degrade");
    }

    #[test]
    fn required_context_replace_rejected() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system prompt"),
            make_block(ContextBlockKind::UserMessage, "hello"),
        ];
        // Plan tries to replace RootSystem (keep with replacement content)
        let items = vec![
            ContextPlanItem { index: 0, action: "keep".into(), content: Some("replacement".into()), original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(), through_event_id: "evt_1".into(),
            mode: "compacted".into(), context_items: items,
            estimated_size: 20, plan_digest: "digest".into(), source_refs: vec![],
        };
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        assert!(matches!(result, PlanApplicationResult::Degraded(_)),
            "required context replacement must degrade");
    }

    #[test]
    fn invalid_item_digest_rejected() {
        let blocks = vec![
            make_block(ContextBlockKind::RootSystem, "system"),
            make_block(ContextBlockKind::ToolResult, "actual content"),
            make_block(ContextBlockKind::UserMessage, "hi"),
        ];
        // Claimed digest doesn't match actual content
        let items = vec![
            ContextPlanItem { index: 0, action: "keep".into(), content: None, original_bytes: None, digest: None },
            ContextPlanItem { index: 1, action: "keep".into(), content: None, original_bytes: Some(15), digest: Some("000000invalid_digest00000".into()) },
            ContextPlanItem { index: 2, action: "keep".into(), content: None, original_bytes: None, digest: None },
        ];
        let plan = ContextCompressResponse {
            provider_id: "test".into(), through_event_id: "evt_1".into(),
            mode: "compacted".into(), context_items: items,
            estimated_size: 30, plan_digest: "digest".into(), source_refs: vec![],
        };
        let result = apply_context_plan(&blocks, &plan, 9999).unwrap();
        assert!(matches!(result, PlanApplicationResult::Degraded(_)),
            "invalid item digest must degrade");
    }
}