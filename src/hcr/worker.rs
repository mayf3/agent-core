//! HCR Worker entry point.
//!
//! v0 minimal flow:
//! 1. Load HCR (read-only).
//! 2. Atomic claim.
//! 3. Idempotent Run creation with RunMode::Hcr.
//! 4. Execute via existing Runtime path (returns structured result).
//!
//! R3 will add settle logic; R4 will add final Feishu reply.

use crate::domain::*;
use crate::hcr::revalidate;
use crate::journal::JournalStore;
use anyhow::Result;

/// Outcome of an HCR execution attempt.
#[derive(Debug, Clone)]
pub struct HcrExecutionOutcome {
    pub claim_id: ClaimId,
    pub run_id: RunId,
    pub is_resume: bool,
}

/// Execute an HCR: claim, create Run binding, and return the outcome.
///
/// This is the internal worker entry point. Steps:
///
/// 1. Load the HCR (read-only, no side effects).
/// 2. Revalidate the HCR principal context (Feishu, p2p).
/// 3. Atomic claim (first stateful action).
/// 4. Idempotent Run binding creation.
///
/// The caller is responsible for executing the Run through the existing
/// Runtime path (tool recall loop via Gateway/Policy/Receipt).
pub fn execute_hcr(
    journal: &JournalStore,
    hcr_id: &str,
    run_id: &RunId,
    worker_instance_id: &str,
) -> Result<HcrExecutionOutcome> {
    // 1. Read-only load (no side effects).
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("HCR_WORKER_LOAD_FAILED: HCR not found: {hcr_id}"))?;

    // 2. Revalidate principal context.
    revalidate::revalidate_hcr_principal(&hcr)?;

    // 3. Atomic claim (first stateful action).
    let harness_id = &hcr.harness_id;
    let claim_id = journal
        .claim_hcr_for_execution(hcr_id, harness_id, worker_instance_id)
        .map_err(|e| {
            // Map inner error to stable category.
            let msg = e.to_string();
            if msg.contains("HCR_ALREADY_CLAIMED") || msg.contains("HCR_NOT_CLAIMABLE") {
                anyhow::anyhow!("HCR_WORKER_CLAIM_FAILED: {msg}")
            } else {
                anyhow::anyhow!("HCR_WORKER_CLAIM_ERROR: {msg}")
            }
        })?;

    // 4. Idempotent Run binding creation.
    let (returned_run_id, is_resume) =
        journal.create_hcr_run_binding(hcr_id, &claim_id.0, &run_id.0)?;

    let actual_run_id = RunId(returned_run_id);

    // If a different run_id was returned (resume), use that.
    let run_id_to_use = if is_resume {
        actual_run_id
    } else {
        run_id.clone()
    };

    Ok(HcrExecutionOutcome {
        claim_id,
        run_id: run_id_to_use,
        is_resume,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_rejects_nonexistent_hcr() {
        let j = JournalStore::in_memory().unwrap();
        let result = execute_hcr(&j, "nonexistent", &RunId::new(), "worker_test");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("HCR_WORKER_LOAD_FAILED"),
            "expected load failure, got: {msg}"
        );
    }

    #[test]
    fn worker_rejects_group_chat_hcr() {
        let j = JournalStore::in_memory().unwrap();
        let (hcr_id, _) = j
            .create_harness_change_request(
                "Feishu",
                "msg_group",
                "sess_1",
                "feishu:open_id:owner",
                "Feishu",
                "group",
                "test-harness",
                "build",
            )
            .unwrap();

        let result = execute_hcr(&j, &hcr_id, &RunId::new(), "worker_test");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("HCR_PRINCIPAL_REJECTED"),
            "expected principal rejection for group chat, got: {msg}"
        );
    }

    #[test]
    fn worker_claims_and_binds_run() {
        let j = JournalStore::in_memory().unwrap();
        let (hcr_id, _) = j
            .create_harness_change_request(
                "Feishu",
                "msg_ok",
                "sess_1",
                "feishu:open_id:owner",
                "Feishu",
                "p2p",
                "test-harness",
                "build",
            )
            .unwrap();

        let run_id = RunId::new();
        let outcome = execute_hcr(&j, &hcr_id, &run_id, "worker_test").unwrap();

        assert!(!outcome.is_resume);
        assert_eq!(outcome.run_id, run_id);

        // Verify claim + binding exist.
        assert_eq!(j.hcr_claim_count().unwrap(), 1);
        assert_eq!(j.hcr_run_binding_count().unwrap(), 1);

        let hcr = j.get_harness_change_request(&hcr_id).unwrap().unwrap();
        assert_eq!(hcr.status, "running");
    }

    #[test]
    fn worker_idempotent_returns_same_outcome() {
        let j = JournalStore::in_memory().unwrap();
        let (hcr_id, _) = j
            .create_harness_change_request(
                "Feishu",
                "msg_idem",
                "sess_1",
                "feishu:open_id:owner",
                "Feishu",
                "p2p",
                "test-harness",
                "build",
            )
            .unwrap();

        let run_id = RunId::new();
        let first = execute_hcr(&j, &hcr_id, &run_id, "worker_test").unwrap();
        assert!(!first.is_resume);

        // Second call should resume (same run_id).
        let second = execute_hcr(&j, &hcr_id, &run_id, "worker_test");
        assert!(second.is_err(), "second claim should fail");
        let msg = second.unwrap_err().to_string();
        assert!(
            msg.contains("HCR_WORKER_CLAIM_FAILED"),
            "expected claim failure on second call, got: {msg}"
        );

        // Still only one claim and one binding.
        assert_eq!(j.hcr_claim_count().unwrap(), 1);
        assert_eq!(j.hcr_run_binding_count().unwrap(), 1);
    }
}
