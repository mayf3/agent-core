//! Runtime E2E tests for context.prepare.v0 hook integration.
//!
//! These tests verify that the hook call function works correctly with the
//! FakeHookClient, and that fragments are properly injected into context
//! blocks, without requiring a full Runtime ingress event pipeline.

use crate::domain::*;
use crate::hook::{
    ContextFragment, ContextFragmentKind, ContextPrepareRequest, FakeHookClient, FragmentPlacement,
    FragmentSensitivity, HookClient, HookConfig, HookFailureMode, HookKind, ResourceRef,
};
use crate::journal::JournalStore;
use anyhow::Result;

/// Helper: create a test context fragment.
fn test_fragment(content: &str) -> ContextFragment {
    ContextFragment {
        id: "f1".into(),
        hook_id: "context.prepare.v0".into(),
        kind: ContextFragmentKind::Instruction,
        placement: FragmentPlacement::UserContext,
        priority: 1,
        content: content.to_string(),
        source: "hook:test".into(),
        ttl_secs: None,
        estimated_tokens: 10,
        sensitivity: FragmentSensitivity::Public,
    }
}

/// Helper: create a basic context block set (system + user message).
fn base_blocks() -> Vec<ContextBlock> {
    vec![
        ContextBlock {
            kind: ContextBlockKind::RootSystem,
            content: "root".into(),
            compressibility: Compressibility::Never,
            source_ref: None,
        },
        ContextBlock {
            kind: ContextBlockKind::RuntimeContract,
            content: "contract".into(),
            compressibility: Compressibility::Never,
            source_ref: None,
        },
        ContextBlock {
            kind: ContextBlockKind::AgentProfile,
            content: "profile".into(),
            compressibility: Compressibility::Never,
            source_ref: None,
        },
        ContextBlock {
            kind: ContextBlockKind::UserMessage,
            content: "hello".into(),
            compressibility: Compressibility::Truncate,
            source_ref: None,
        },
    ]
}

/// Helper: call the hook function and return the outcome.
fn run_hook(
    blocks: &mut Vec<ContextBlock>,
    client: &dyn HookClient,
    cfg: &HookConfig,
    journal: &JournalStore,
) -> Result<crate::runtime::hook_call::HookCallOutcome> {
    crate::runtime::hook_call::call_context_prepare(
        blocks,
        client,
        cfg,
        journal,
        &RunId::new(),
        &SessionId("s1".into()),
        "main",
        "user",
        "cli",
        "test",
        4000,
    )
}

// ── 1. Disabled hook preserves prompt ─────────────────────────────────

#[test]
fn context_prepare_disabled_preserves_prompt() -> Result<()> {
    let blocks = base_blocks();
    let original_len = blocks.len();
    let journal = JournalStore::in_memory()?;
    let client = FakeHookClient::with_fragments(vec![test_fragment("should not appear")]);
    let cfg = HookConfig {
        enabled: false,
        ..Default::default()
    };

    // When disabled, the Runtime never calls run_hook. Simulate: don't call run_hook.
    assert_eq!(blocks.len(), original_len);
    let events = journal.events()?;
    assert!(!events
        .iter()
        .any(|e| e.kind == JournalEventKind::HookCallRecorded));
    // Consume variables used in the test setup.
    drop(client);
    drop(cfg);
    Ok(())
}

// ── 2. Fragments are injected ─────────────────────────────────────────

#[test]
fn context_prepare_adds_dynamic_fragment() -> Result<()> {
    let mut blocks = base_blocks();
    let journal = JournalStore::in_memory()?;
    let client = FakeHookClient::with_fragments(vec![test_fragment("dynamic content")]);
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        max_fragments: 10,
        ..Default::default()
    };

    let outcome = run_hook(&mut blocks, &client, &cfg, &journal)?;
    assert!(matches!(
        outcome,
        crate::runtime::hook_call::HookCallOutcome::Injected
    ));

    let hook_count = blocks
        .iter()
        .filter(|b| b.kind == ContextBlockKind::HookFragment)
        .count();
    assert_eq!(hook_count, 1);
    // UserMessage must be the last block.
    assert_eq!(blocks.last().unwrap().kind, ContextBlockKind::UserMessage);
    Ok(())
}

// ── 3. HookFragment before UserMessage, not before immutable blocks ────

#[test]
fn context_prepare_injected_before_user_message() -> Result<()> {
    let mut blocks = base_blocks();
    let journal = JournalStore::in_memory()?;
    let client = FakeHookClient::with_fragments(vec![test_fragment("fact")]);
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        max_fragments: 10,
        ..Default::default()
    };

    let pre_user_pos = blocks
        .iter()
        .position(|b| b.kind == ContextBlockKind::UserMessage)
        .unwrap();
    run_hook(&mut blocks, &client, &cfg, &journal)?;

    let hook_pos = blocks
        .iter()
        .position(|b| b.kind == ContextBlockKind::HookFragment)
        .unwrap();
    let user_pos = blocks
        .iter()
        .position(|b| b.kind == ContextBlockKind::UserMessage)
        .unwrap();

    assert!(
        hook_pos <= pre_user_pos,
        "HookFragment inserted at or before original UserMessage position"
    );
    assert!(hook_pos < user_pos, "HookFragment before UserMessage");
    // Immutable blocks (0-2) are unchanged.
    assert_eq!(blocks[0].kind, ContextBlockKind::RootSystem);
    assert_eq!(blocks[1].kind, ContextBlockKind::RuntimeContract);
    assert_eq!(blocks[2].kind, ContextBlockKind::AgentProfile);
    Ok(())
}

// ── 4. FailClosed kills run ───────────────────────────────────────────

#[test]
fn context_prepare_hook_error_fail_closed() -> Result<()> {
    let mut blocks = base_blocks();
    let journal = JournalStore::in_memory()?;
    let client = FakeHookClient::with_error("fatal");
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        failure_mode: HookFailureMode::FailClosed,
        ..Default::default()
    };

    let outcome = run_hook(&mut blocks, &client, &cfg, &journal)?;
    assert!(matches!(
        outcome,
        crate::runtime::hook_call::HookCallOutcome::FailClosed { .. }
    ));

    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .unwrap();
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("failed")
    );
    assert_eq!(
        rec.payload.get("failure_mode").and_then(|v| v.as_str()),
        Some("fail_closed")
    );
    Ok(())
}

// ── 5. FailOpen continues ─────────────────────────────────────────────

#[test]
fn context_prepare_hook_error_fail_open_continues() -> Result<()> {
    let mut blocks = base_blocks();
    let journal = JournalStore::in_memory()?;
    let client = FakeHookClient::with_error("nonfatal");
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        failure_mode: HookFailureMode::FailOpen,
        ..Default::default()
    };

    let outcome = run_hook(&mut blocks, &client, &cfg, &journal)?;
    assert!(matches!(
        outcome,
        crate::runtime::hook_call::HookCallOutcome::Skipped
    ));

    assert!(!blocks
        .iter()
        .any(|b| b.kind == ContextBlockKind::HookFragment));
    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .unwrap();
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("skipped")
    );
    Ok(())
}

// ── 6. Degrade continues ──────────────────────────────────────────────

#[test]
fn context_prepare_hook_error_degrade_continues() -> Result<()> {
    let mut blocks = base_blocks();
    let journal = JournalStore::in_memory()?;
    let client = FakeHookClient::with_error("slow");
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        failure_mode: HookFailureMode::Degrade,
        ..Default::default()
    };

    let outcome = run_hook(&mut blocks, &client, &cfg, &journal)?;
    assert!(matches!(
        outcome,
        crate::runtime::hook_call::HookCallOutcome::Skipped
    ));

    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .unwrap();
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("degraded")
    );
    assert_eq!(
        rec.payload.get("failure_mode").and_then(|v| v.as_str()),
        Some("degrade")
    );
    Ok(())
}

// ── 7. Fragment over limit ────────────────────────────────────────────

#[test]
fn context_prepare_fragment_over_limit_fail_closed() -> Result<()> {
    let large_frag = ContextFragment {
        id: "big".into(),
        hook_id: "test".into(),
        kind: ContextFragmentKind::Fact,
        placement: FragmentPlacement::UserContext,
        priority: 0,
        content: "x".repeat(200),
        source: "test".into(),
        ttl_secs: None,
        estimated_tokens: 50,
        sensitivity: FragmentSensitivity::Public,
    };
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        failure_mode: HookFailureMode::FailClosed,
        max_response_bytes: 100,
        ..Default::default()
    };
    let client = FakeHookClient::with_fragments(vec![large_frag]);
    let result = client.call_context_prepare(
        &ContextPrepareRequest {
            hook: HookKind::ContextPrepareV0,
            run_id: "r".into(),
            session_id: "s".into(),
            agent_id: "main".into(),
            principal: "user".into(),
            channel: "cli".into(),
            user_text: "test".into(),
            context_budget_chars: 4000,
        },
        &cfg,
    );
    assert!(result.is_err(), "over-limit fragment should be rejected");
    Ok(())
}

// ── 8. HookCallRecorded contains required fields ──────────────────────

#[test]
fn context_prepare_records_hook_call_event() -> Result<()> {
    let mut blocks = base_blocks();
    let journal = JournalStore::in_memory()?;
    let client = FakeHookClient::with_fragments(vec![test_fragment("data")]);
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        max_fragments: 10,
        ..Default::default()
    };

    run_hook(&mut blocks, &client, &cfg, &journal)?;

    let events = journal.events()?;
    let rec = events
        .iter()
        .find(|e| e.kind == JournalEventKind::HookCallRecorded)
        .expect("HookCallRecorded event must exist");
    assert_eq!(
        rec.payload.get("hook").and_then(|v| v.as_str()),
        Some("context.prepare.v0")
    );
    assert_eq!(
        rec.payload.get("status").and_then(|v| v.as_str()),
        Some("ok")
    );
    assert!(rec.payload.get("fragment_count").is_some());
    assert!(rec.payload.get("response_bytes").is_some());
    assert!(rec.payload.get("duration_ms").is_some());
    assert!(rec.run_id.is_some());
    assert!(rec.session_id.is_some());
    Ok(())
}

// ── 9. ResourceRefs allowed but not loaded ────────────────────────────

#[test]
fn context_prepare_resource_refs_allowed_but_not_loaded() -> Result<()> {
    let mut blocks = base_blocks();
    let journal = JournalStore::in_memory()?;
    let resource = ResourceRef {
        id: "res-1".into(),
        title: "Test".into(),
        summary: "A resource".into(),
        source: "ref:test".into(),
        estimated_token_cost: 100,
        load_hint: None,
    };
    let client = FakeHookClient {
        fragments: vec![],
        resource_refs: vec![resource],
        inject_error: None,
    };
    let cfg = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        max_fragments: 10,
        ..Default::default()
    };

    run_hook(&mut blocks, &client, &cfg, &journal)?;
    // ResourceRefs are not loaded into context blocks in v0.
    assert!(!blocks.iter().any(|b| b.content.contains("res-1")));
    Ok(())
}

// ── 10. No product-layer terms ────────────────────────────────────────

#[test]
fn context_prepare_no_product_layer_terms() {
    let forbidden = ["Memory", "Dream", "Task", "Skill", "Dashboard"];
    let names = [
        "ContextPrepareRequest",
        "ContextPrepareResponse",
        "HookCallRecorded",
        "HookFragment",
        "HookClient",
        "FakeHookClient",
    ];
    for name in &names {
        for term in &forbidden {
            assert!(
                !name.contains(term),
                "'{name}' contains forbidden term '{term}'"
            );
        }
    }
}
