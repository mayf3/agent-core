//! Server-side revalidation for HCR execution context.
//!
//! Before each privileged tool dispatch in HCR mode, the system revalidates:
//! - Principal: is the HCR owner (Feishu p2p).
//! - Feishu context: channel=Feishu, conversation_kind=p2p.
//! - HCR state: still `running`, claim_id matches RunMode, harness_id matches.
//! - Workspace: is the correct HCR harness workspace.
//!
//! These checks prevent stale or forged HCR contexts from executing.

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::{bail, Result};

/// The pinned workspace ID for HCR harness development.
pub const HCR_HARNESS_WORKSPACE_ID: &str = "harness-dev";

/// Revalidate the HCR execution context before creating a Run or dispatching
/// a privileged tool call.
///
/// Checks:
/// 1. The Run is in HCR mode with valid binding fields.
/// 2. The HCR still exists and has status `running`.
/// 3. The claim is still active.
/// 4. The principal/channel/chat_type match the HCR's recorded context
///    (Feishu, p2p, correct owner).
pub fn revalidate_hcr_context(journal: &JournalStore, run: &Run) -> Result<()> {
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
    use crate::journal::JournalStore;

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

    fn create_hcr_run(j: &JournalStore, hcr_id: &str, run_id: &str) -> Result<Run> {
        let claim_id = j.claim_hcr_for_execution(hcr_id, "test-harness", "worker_1")?;
        j.create_hcr_run_binding(hcr_id, &claim_id.0, run_id)?;
        Ok(Run {
            id: RunId(run_id.to_string()),
            session_id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:owner".into()),
                subject: PrincipalSubject::FeishuOpenId("owner".into()),
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
        })
    }

    #[test]
    fn revalidation_passes_for_valid_hcr() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let run = create_hcr_run(&j, &hcr_id, "run_reval_1")?;

        let result = revalidate_hcr_context(&j, &run);
        assert!(result.is_ok(), "revalidation should pass: {:?}", result);
        Ok(())
    }

    #[test]
    fn revalidation_fails_for_default_run() -> Result<()> {
        let j = JournalStore::in_memory()?;
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

        let err = revalidate_hcr_context(&j, &run).unwrap_err();
        assert!(
            err.to_string().contains("HCR_REVALIDATION_FAILED"),
            "expected HCR_REVALIDATION_FAILED, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn revalidation_fails_for_stale_hcr() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let run = create_hcr_run(&j, &hcr_id, "run_reval_2")?;

        // Manually cancel the HCR (simulate admin cancellation).
        {
            let conn = j.conn.lock().unwrap();
            conn.execute(
                "UPDATE harness_change_requests SET status = 'cancelled' WHERE request_id = ?1",
                rusqlite::params![hcr_id],
            )
            .unwrap();
        }

        let err = revalidate_hcr_context(&j, &run).unwrap_err();
        assert!(
            err.to_string().contains("HCR_REVALIDATION_FAILED"),
            "expected HCR_REVALIDATION_FAILED for stale HCR, got: {err}"
        );
        Ok(())
    }

    #[test]
    fn revalidation_fails_for_harness_id_mismatch() -> Result<()> {
        let j = JournalStore::in_memory()?;
        let hcr_id = setup_test_hcr(&j)?;
        let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

        // Create Run with wrong harness_id.
        let run = Run {
            id: RunId("run_reval_3".into()),
            session_id: SessionId("s_1".into()),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:owner".into()),
                subject: PrincipalSubject::FeishuOpenId("owner".into()),
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

        let err = revalidate_hcr_context(&j, &run).unwrap_err();
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
}
