//! Preflight & validation regression tests.

use anyhow::Result;

// ---------------------------------------------------------------------------
// coding_router_rejects_missing_contracts
// ---------------------------------------------------------------------------

#[test]
fn coding_router_requires_development_verb() -> Result<()> {
    use agent_core_kernel::server::coding_router;

    let result = coding_router::parse_coding_intent("列出所有组件");
    assert!(result.is_err(), "expected non-development intent to be rejected");
    Ok(())
}

#[test]
fn coding_router_requires_known_contract() -> Result<()> {
    use agent_core_kernel::server::coding_router;

    let result = coding_router::parse_coding_intent("开发一个 my-component");
    assert!(result.is_err(), "expected unknown contract to be rejected");
    Ok(())
}

#[test]
fn coding_router_accepts_known_contract() -> Result<()> {
    use agent_core_kernel::server::coding_router;

    let result = coding_router::parse_coding_intent(
        "开发一个 failure-viewer，通过 event.observe.v0 监控失败事件",
    );
    assert!(result.is_ok(), "expected known contract to parse");
    let intent = result?;
    assert!(
        intent.development_request.required_contracts.contains(&"event.observe.v0".to_string()),
        "parsed intent must contain event.observe.v0 in required_contracts"
    );
    Ok(())
}

#[test]
fn coding_router_rejects_shell_injection() -> Result<()> {
    use agent_core_kernel::server::coding_router;

    let result = coding_router::parse_coding_intent(
        "开发一个安全组件； rm -rf / ; event.observe.v0",
    );
    assert!(result.is_err(), "expected shell injection attempt to be rejected");
    Ok(())
}

// ---------------------------------------------------------------------------
// outbox recovery queries
// ---------------------------------------------------------------------------

#[test]
fn undelivered_ingress_query_works() -> Result<()> {
    use agent_core_kernel::journal::JournalStore;

    let journal = JournalStore::in_memory()?;
    let undelivered = journal.undelivered_ingress_events()?;
    assert!(undelivered.is_empty(), "fresh journal must have 0 undelivered events");
    Ok(())
}
