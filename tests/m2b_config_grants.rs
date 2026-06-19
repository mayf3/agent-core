//! Phase 2 M2b config-driven grants: a run principal receives its channel's
//! baseline grant plus any operator-configured extra catalog operations.
//!
//! See `docs/decisions/phase2-invocation-gateway-scoping.md` (M2b) and
//! `src/domain/operation.rs` (`ExecutionProfile::with_extra`).

mod common;

use agent_core_kernel::domain::operation::FEISHU_SEND_MESSAGE;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use anyhow::Result;

#[test]
fn gateway_cli_ingress_grants_configured_extra_operations() -> Result<()> {
    // Phase 2 M2b config-driven half: when the operator configures extra
    // catalog operations, a CLI ingress principal receives them in addition
    // to its channel baseline grant. The principal may then be approved for
    // those operations (the gateway allowlist is the catalog).
    let mut config = common::test_config();
    config.extra_allowed_operations = vec![FEISHU_SEND_MESSAGE.to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let event = gateway.validate_ingress(&journal, gateway.cli_ingress("hi".to_string())?)?;
    let operations: Vec<&str> = event
        .principal
        .grants
        .iter()
        .map(|g| g.operation.as_str())
        .collect();
    assert!(
        operations.contains(&"stdout.send_text"),
        "baseline cli grant kept"
    );
    assert!(
        operations.contains(&FEISHU_SEND_MESSAGE),
        "configured extra operation granted"
    );
    Ok(())
}

#[test]
fn gateway_cli_ingress_drops_uncatalogued_extra_operations() -> Result<()> {
    // Operations not in the catalog can never be approved (the gateway
    // allowlist is the catalog), so they must not appear as grants even when
    // an operator mistakenly lists them.
    let mut config = common::test_config();
    config.extra_allowed_operations = vec!["shell.exec".to_string()];
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config);
    let event = gateway.validate_ingress(&journal, gateway.cli_ingress("hi".to_string())?)?;
    let operations: Vec<&str> = event
        .principal
        .grants
        .iter()
        .map(|g| g.operation.as_str())
        .collect();
    assert_eq!(operations, vec!["stdout.send_text", "session.recall_recent"]);
    Ok(())
}
