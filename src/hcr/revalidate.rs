//! Server-side revalidation for HCR execution context.
//!
//! Before each privileged tool dispatch in HCR mode, the system revalidates:
//! - HCR state: still `running`, claim_id matches RunMode, harness_id matches.
//! - Principal: the Run principal matches the HCR's bound principal.
//! - Owner status: the principal is still the configured coding owner.
//! - Feishu context: channel=Feishu, conversation_kind=p2p, conversation
//!   identifier matches the HCR's recorded context.
//! - Workspace: is the correct HCR harness workspace.

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{bail, Result};

/// The pinned workspace ID for HCR harness development.
pub const HCR_HARNESS_WORKSPACE_ID: &str = "harness-dev";

/// Per-dispatch revalidation of the HCR execution context.
///
/// Called before every privileged HCR Coding Harness dispatch. Checks:
///
/// 1. RunMode::Hcr with valid hcr_id, harness_id, claim_id.
/// 2. HCR exists and is still in `running` status.
/// 3. harness_id matches between HCR and RunMode.
/// 4. Active claim exists and claim_id matches.
/// 5. Principal identity: Run principal matches HCR's bound principal.
/// 6. Channel: Session is Feishu.
/// 7. Chat type: HCR was created in p2p (private chat).
/// 8. Conversation identity: session conversation_key matches HCR's
///    bound principal_id (for Feishu p2p: both are feishu:open_id:{id}).
///
/// `is_owner` should be computed by the caller via
/// `runtime::coding_grants::is_coding_owner` to avoid circular deps.
pub fn revalidate_hcr_dispatch_context(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    is_owner: bool,
) -> Result<()> {
    // ── HCR/claim/binding checks ─────────────────────────────────────

    // 1. Verify RunMode::Hcr with valid fields.
    let (hcr_id, harness_id, claim_id) = match &run.mode {
        RunMode::Hcr {
            hcr_id,
            harness_id,
            claim_id,
        } => (hcr_id.as_str(), harness_id.as_str(), claim_id.as_str()),
        _ => {
            bail!("HCR_REVALIDATION_FAILED: run is not in HCR mode");
        }
    };

    if hcr_id.is_empty() || harness_id.is_empty() || claim_id.is_empty() {
        bail!("HCR_REVALIDATION_FAILED: incomplete HCR binding fields");
    }

    // 2. Load the HCR and verify it is still running.
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("HCR_REVALIDATION_FAILED: HCR not found: {hcr_id}"))?;

    if hcr.status != "running" {
        bail!(
            "HCR_REVALIDATION_FAILED: HCR status is {}, expected running",
            hcr.status
        );
    }

    // 3. Verify harness_id matches.
    if hcr.harness_id != harness_id {
        bail!(
            "HCR_REVALIDATION_FAILED: harness_id mismatch: HCR has {}, RunMode has {}",
            hcr.harness_id,
            harness_id
        );
    }

    // 4. Verify the claim is still active.
    let claim = journal.get_active_claim_for_hcr(hcr_id)?.ok_or_else(|| {
        anyhow::anyhow!("HCR_REVALIDATION_FAILED: no active claim for HCR {hcr_id}")
    })?;

    if claim.claim_id.0 != claim_id {
        bail!(
            "HCR_REVALIDATION_FAILED: claim_id mismatch: stored {}, RunMode has {}",
            claim.claim_id.0,
            claim_id
        );
    }

    // ── Per-dispatch principal/context checks (H1 fix) ───────────────

    // 5. Principal identity: Run principal must match HCR's bound principal.
    if run.principal.principal_id.0 != hcr.principal_id {
        bail!(
            "HCR_DISPATCH_REJECTED: principal_id mismatch: Run has {}, HCR has {}",
            run.principal.principal_id.0,
            hcr.principal_id
        );
    }

    // 6. Channel: Session must be Feishu.
    if session.channel != ChannelKind::Feishu {
        bail!(
            "HCR_DISPATCH_REJECTED: channel is {:?}, expected Feishu",
            session.channel
        );
    }

    // 7. Chat type: HCR must have been created in a private chat.
    if hcr.chat_type != "p2p" {
        bail!(
            "HCR_DISPATCH_REJECTED: HCR chat_type is {}, expected p2p",
            hcr.chat_type
        );
    }

    // 8. Owner status: principal must still be the configured coding owner.
    if !is_owner {
        bail!("HCR_DISPATCH_REJECTED: principal is no longer the coding owner");
    }

    // 9. Conversation identity: session conversation must match HCR's
    //    bound principal. For Feishu p2p, conversation_key == principal_id
    //    (both are "feishu:open_id:{open_id}").
    if session.conversation_key != hcr.principal_id {
        bail!(
            "HCR_DISPATCH_REJECTED: conversation '{}' does not match HCR principal '{}'",
            session.conversation_key,
            hcr.principal_id
        );
    }

    Ok(())
}

/// Verify that the workspace ID in a tool argument matches the HCR's pinned
/// harness workspace. Rejects empty, absolute, or traversal paths.
pub fn validate_hcr_workspace(workspace_id: &str) -> Result<()> {
    if workspace_id.is_empty() {
        bail!("HCR_WORKSPACE_REJECTED: workspace_id is empty");
    }
    if workspace_id != HCR_HARNESS_WORKSPACE_ID {
        bail!(
            "HCR_WORKSPACE_REJECTED: workspace_id '{}' is not the HCR harness workspace '{}'",
            workspace_id,
            HCR_HARNESS_WORKSPACE_ID
        );
    }
    Ok(())
}

/// Verify the principal/channel/chat_type match the HCR's recorded context.
/// The HCR must have been created by the Feishu coding owner in a private chat.
/// Called at worker entry (claim time) as an early gate.
pub fn revalidate_hcr_principal(hcr: &HarnessChangeRequest) -> Result<()> {
    if hcr.channel != "Feishu" {
        bail!(
            "HCR_PRINCIPAL_REJECTED: channel is {}, expected Feishu",
            hcr.channel
        );
    }
    if hcr.chat_type != "p2p" {
        bail!(
            "HCR_PRINCIPAL_REJECTED: chat_type is {}, expected p2p",
            hcr.chat_type
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KernelConfig;
    use crate::domain::*;
    use crate::journal::JournalStore;
    use crate::runtime::coding_grants::is_coding_owner;

    fn setup_test_hcr(j: &JournalStore) -> Result<String> {
        let (request_id, _) = j.create_harness_change_request(
            "Feishu",
            "test_msg_reval",
            "session_1",
            "feishu:open_id:owner",
            "Feishu",
            "p2p",
            "test-harness",
            "build test environment",
        )?;
        Ok(request_id)
    }

    fn setup_config() -> KernelConfig {
        let mut c = KernelConfig::from_cli(None);
        // Set a known owner for testing.
        c.feishu_coding_owner_id = Some("feishu:open_id:owner".to_string());
        c
    }

    fn is_owner_for_test(config: &KernelConfig, principal: &RunPrincipal) -> bool {
        is_coding_owner(config, principal, Some("p2p"))
    }

    fn create_hcr_run(j: &JournalStore, hcr_id: &str, run_id: &str) -> Result<(Run, Session)> {
        let claim_id = j.claim_hcr_for_execution(hcr_id, "test-harness", "worker_1")?;
        j.create_hcr_run_binding(hcr_id, &claim_id.0, run_id)?;
        let run = Run {
            id: RunId(run_id.to_string()),
            session_id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:owner".into()),
                subject: PrincipalSubject::FeishuOpenId("feishu:open_id:owner".into()),
                source: PrincipalSource::Feishu,
                grants: vec![],
                requester_id: Some("feishu:open_id:owner".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            registry_snapshot_id: "snap_test".into(),
            mode: RunMode::Hcr {
                hcr_id: hcr_id.to_string(),
                harness_id: "test-harness".to_string(),
                claim_id: claim_id.0,
            },
        };
        let session = Session {
            id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:owner".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: chrono::Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };
        Ok((run, session))
    }

    #[test]
    fn dispatch_revalidation_passes_for_valid_hcr() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let config = setup_config();
        let (run, session) = create_hcr_run(&j, &hcr_id, "run_reval_1")?;
        let is_owner = is_owner_for_test(&config, &run.principal);

        let result = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner);
        assert!(result.is_ok(), "revalidation should pass: {:?}", result);
        Ok(())
    }

    #[test]
    fn dispatch_revalidation_fails_for_default_run() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let config = setup_config();
        let run = Run {
            id: RunId::new(),
            session_id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: Some("cli:local".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            registry_snapshot_id: String::new(),
            mode: RunMode::Default,
        };
        let session = Session {
            id: SessionId("s_cli".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "local".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: chrono::Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, false).unwrap_err();
        assert!(
            err.to_string().contains("HCR_REVALIDATION_FAILED"),
            "expected HCR_REVALIDATION_FAILED, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn dispatch_revalidation_fails_wrong_principal() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let config = setup_config();
        let (mut run, session) = create_hcr_run(&j, &hcr_id, "run_wrong_principal")?;
        // Change the Run's principal to a different user.
        run.principal.principal_id = PrincipalId("feishu:open_id:different_user".into());
        run.principal.subject =
            PrincipalSubject::FeishuOpenId("feishu:open_id:different_user".into());
        let is_owner = is_owner_for_test(&config, &run.principal);

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner).unwrap_err();
        assert!(
            err.to_string().contains("HCR_DISPATCH_REJECTED"),
            "expected HCR_DISPATCH_REJECTED for wrong principal, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn dispatch_revalidation_fails_non_feishu_session() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let config = setup_config();
        let (run, mut session) = create_hcr_run(&j, &hcr_id, "run_non_feishu")?;
        session.channel = ChannelKind::Cli;
        session.conversation_key = "local".into();
        let is_owner = is_owner_for_test(&config, &run.principal);

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner).unwrap_err();
        assert!(
            err.to_string().contains("HCR_DISPATCH_REJECTED"),
            "expected HCR_DISPATCH_REJECTED for non-Feishu session, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn dispatch_revalidation_fails_wrong_conversation() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let config = setup_config();
        let (run, mut session) = create_hcr_run(&j, &hcr_id, "run_wrong_conv")?;
        // Different conversation key (different p2p chat).
        session.conversation_key = "feishu:open_id:different_user".into();
        let is_owner = is_owner_for_test(&config, &run.principal);

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner).unwrap_err();
        assert!(
            err.to_string().contains("HCR_DISPATCH_REJECTED"),
            "expected HCR_DISPATCH_REJECTED for wrong conversation, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn dispatch_revalidation_fails_non_owner() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let config = setup_config();
        let (run, session) = create_hcr_run(&j, &hcr_id, "run_non_owner")?;
        // Not the owner: pass is_owner=false even though principal matches.
        let is_owner = false;

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner).unwrap_err();
        assert!(
            err.to_string().contains("HCR_DISPATCH_REJECTED"),
            "expected HCR_DISPATCH_REJECTED for non-owner, got: {err}"
        );
        assert!(
            err.to_string().contains("no longer the coding owner"),
            "expected owner rejection message"
        );
        Ok(())
    }

    #[test]
    fn dispatch_revalidation_fails_for_stale_hcr() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let config = setup_config();
        let (run, session) = create_hcr_run(&j, &hcr_id, "run_reval_2")?;
        let is_owner = is_owner_for_test(&config, &run.principal);

        // Manually cancel the HCR (simulate admin cancellation).
        {
            let conn = j.conn.lock().unwrap();
            conn.execute(
                "UPDATE harness_change_requests SET status = 'cancelled' WHERE request_id = ?1",
                rusqlite::params![hcr_id],
            )
            .unwrap();
        }

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner).unwrap_err();
        assert!(
            err.to_string().contains("HCR_REVALIDATION_FAILED"),
            "expected HCR_REVALIDATION_FAILED for stale HCR, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn dispatch_revalidation_fails_for_harness_id_mismatch() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let config = setup_config();
        let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

        // Create Run with wrong harness_id.
        let run = Run {
            id: RunId("run_reval_3".into()),
            session_id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:owner".into()),
                subject: PrincipalSubject::FeishuOpenId("feishu:open_id:owner".into()),
                source: PrincipalSource::Feishu,
                grants: vec![],
                requester_id: Some("feishu:open_id:owner".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            registry_snapshot_id: "snap_test".into(),
            mode: RunMode::Hcr {
                hcr_id: hcr_id.clone(),
                harness_id: "wrong-harness".to_string(),
                claim_id: claim_id.0,
            },
        };
        let session = Session {
            id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:owner".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: chrono::Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };
        let is_owner = is_owner_for_test(&config, &run.principal);

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner).unwrap_err();
        assert!(
            err.to_string().contains("HCR_REVALIDATION_FAILED"),
            "expected HCR_REVALIDATION_FAILED for harness mismatch, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn workspace_validation_accepts_correct_id() {
        assert!(validate_hcr_workspace("harness-dev").is_ok());
    }

    #[test]
    fn workspace_validation_rejects_wrong_id() {
        let err = validate_hcr_workspace("other-workspace").unwrap_err();
        assert!(err.to_string().contains("HCR_WORKSPACE_REJECTED"));
    }

    #[test]
    fn workspace_validation_rejects_empty_id() {
        let err = validate_hcr_workspace("").unwrap_err();
        assert!(err.to_string().contains("HCR_WORKSPACE_REJECTED"));
    }

    #[test]
    fn principal_revalidation_accepts_feishu_p2p() {
        let hcr = HarnessChangeRequest {
            request_id: "hcr_test".into(),
            source: "Feishu".into(),
            source_message_id: "msg_1".into(),
            session_id: "s_1".into(),
            principal_id: "feishu:open_id:owner".into(),
            channel: "Feishu".into(),
            chat_type: "p2p".into(),
            harness_id: "test-harness".into(),
            requirement: "build".into(),
            status: "running".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            run_id: None,
            error_code: None,
        };
        assert!(revalidate_hcr_principal(&hcr).is_ok());
    }

    #[test]
    fn principal_revalidation_rejects_group_chat() {
        let hcr = HarnessChangeRequest {
            request_id: "hcr_test".into(),
            source: "Feishu".into(),
            source_message_id: "msg_1".into(),
            session_id: "s_1".into(),
            principal_id: "feishu:open_id:owner".into(),
            channel: "Feishu".into(),
            chat_type: "group".into(),
            harness_id: "test-harness".into(),
            requirement: "build".into(),
            status: "running".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            run_id: None,
            error_code: None,
        };
        let err = revalidate_hcr_principal(&hcr).unwrap_err();
        assert!(err.to_string().contains("HCR_PRINCIPAL_REJECTED"));
    }

    #[test]
    fn dispatch_revalidation_fails_group_chat_hcr() -> Result<()> {
        // Create an HCR that was made in a group chat (should never happen
        // since PR4A1 rejects group chat, but defense in depth).
        let j = JournalStore::in_memory()?;
        let config = setup_config();
        let (hcr_id, _) = j.create_harness_change_request(
            "Feishu",
            "group_msg",
            "session_1",
            "feishu:open_id:owner",
            "Feishu",
            "group",
            "test-harness",
            "group test",
        )?;
        let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;
        j.create_hcr_run_binding(&hcr_id, &claim_id.0, "run_group")?;
        let run = Run {
            id: RunId("run_group".into()),
            session_id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:owner".into()),
                subject: PrincipalSubject::FeishuOpenId("feishu:open_id:owner".into()),
                source: PrincipalSource::Feishu,
                grants: vec![],
                requester_id: Some("feishu:open_id:owner".into()),
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            registry_snapshot_id: "snap_test".into(),
            mode: RunMode::Hcr {
                hcr_id: hcr_id.clone(),
                harness_id: "test-harness".to_string(),
                claim_id: claim_id.0,
            },
        };
        let session = Session {
            id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:owner".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: chrono::Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };
        let is_owner = is_owner_for_test(&config, &run.principal);

        let err = revalidate_hcr_dispatch_context(&j, &run, &session, is_owner).unwrap_err();
        assert!(
            err.to_string().contains("HCR_DISPATCH_REJECTED"),
            "expected HCR_DISPATCH_REJECTED for group HCR, got: {err}"
        );
        assert!(
            err.to_string().contains("p2p"),
            "expected p2p rejection message"
        );
        Ok(())
    }
}
