//! Read-only GET proposal endpoint tests.
//! Split from capability_routes_tests.rs to stay under the 500-line gate.

use super::capability_routes_support::*;
use crate::capabilities::store::ContentStore;
use crate::journal::JournalStore;
use anyhow::Result;

#[test]
fn get_proposal_returns_digests_and_status() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let setup = ProposalSetup::build(PROBE_OP, ENDPOINT, None)?;
    let pid = setup.submit(&journal, &_gw)?;

    let resp = crate::server::capability_routes::handle_get_proposal(&journal, &setup.store, &pid)?;
    assert_eq!(resp["proposal_id"], pid);
    assert_eq!(resp["status"], "PendingApproval");
    assert_eq!(resp["operation_name"], PROBE_OP);
    assert!(!resp["artifact_digest"].as_str().unwrap_or("").is_empty());
    assert!(!resp["manifest_digest"].as_str().unwrap_or("").is_empty());
    assert!(!resp["manifest_id"].as_str().unwrap_or("").is_empty());
    Ok(())
}

#[test]
fn get_proposal_not_found_returns_error() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    let _gw = gateway();
    let store = ContentStore::new("/tmp/nonexistent".into());
    let err = crate::server::capability_routes::handle_get_proposal(
        &journal,
        &store,
        "proposal_nonexistent",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("not_found"), "got: {err}");
    Ok(())
}
