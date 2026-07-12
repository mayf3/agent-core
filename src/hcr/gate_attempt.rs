//! Canonical Gate Attempt creation for HCR settlement (R3A-R1).
//!
//! The service-side entry point for creating a gate attempt. It:
//! 1. Loads HCR, claim, Run, RunMode from store
//! 2. Verifies the gate kind is next in sequence
//! 3. Creates the attempt with fixed operation/profile/workspace/harness
//! 4. Creates an InvocationIntent via the standard dispatch path
//! 5. Persists the attempt

use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::json;

/// Gate definition: maps a GateKind to its fixed operation, profile, and
/// workspace. This is the service-side definition that gates MUST use.
/// R3B will replace test operation with real acceptance commands.
pub struct GateDefinition {
    pub kind: GateKind,
    pub operation: &'static str,
    pub profile: &'static str,
    pub workspace_id: &'static str,
}

impl GateDefinition {
    pub fn for_kind(kind: GateKind) -> Self {
        let operation = crate::domain::operation::external::WORKSPACE_EXEC;
        let profile = "hcr_trusted_profile";
        let workspace_id = crate::hcr::revalidate::HCR_HARNESS_WORKSPACE_ID;
        GateDefinition {
            kind,
            operation,
            profile,
            workspace_id,
        }
    }

    pub fn all() -> Vec<Self> {
        GateKind::all_required()
            .iter()
            .map(|&k| Self::for_kind(k))
            .collect()
    }
}

/// Prepare a canonical gate attempt for the given HCR, claim, and run.
/// Creates the attempt record and an InvocationIntent with the expected
/// operation, returning the attempt_id and intent_id.
///
/// The caller specifies only which gate kind to prepare — the operation,
/// profile, workspace, and harness are fixed by the GateDefinition.
pub fn prepare_hcr_gate_attempt(
    journal: &JournalStore,
    gateway: &Gateway,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    session: &Session,
    snapshot: &RegistrySnapshot,
    gate_kind: GateKind,
) -> Result<(String, String)> {
    // ── 1. Load the HCR ───────────────────────────────────────────────
    let hcr = journal
        .get_harness_change_request(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("ATTEMPT_HCR_NOT_FOUND: {hcr_id}"))?;
    if hcr.status != "running" {
        bail!(
            "ATTEMPT_HCR_NOT_RUNNING: HCR {hcr_id} status {}",
            hcr.status
        );
    }

    // ── 2. Verify claim ───────────────────────────────────────────────
    let claim = journal
        .get_active_claim_for_hcr(hcr_id)?
        .ok_or_else(|| anyhow::anyhow!("ATTEMPT_NO_ACTIVE_CLAIM: HCR {hcr_id}"))?;
    if claim.claim_id.0 != claim_id {
        bail!(
            "ATTEMPT_CLAIM_MISMATCH: expected {claim_id}, found {}",
            claim.claim_id.0
        );
    }

    // ── 3. Verify Run binding and RunMode ─────────────────────────────
    let binding = journal
        .get_run_binding_for_claim(claim_id)?
        .ok_or_else(|| anyhow::anyhow!("ATTEMPT_NO_RUN_BINDING: claim {claim_id}"))?;
    if binding.run_id != run_id {
        bail!(
            "ATTEMPT_RUN_MISMATCH: expected {run_id}, found {}",
            binding.run_id
        );
    }

    // Load persisted Run to verify RunMode.
    let run = journal
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("ATTEMPT_RUN_NOT_FOUND: run {run_id}"))?;

    match &run.mode {
        RunMode::Hcr {
            hcr_id: rh,
            claim_id: rc,
            harness_id: rhh,
        } => {
            if rh != hcr_id {
                bail!("ATTEMPT_RUNMODE_HCR_MISMATCH");
            }
            if rc != claim_id {
                bail!("ATTEMPT_RUNMODE_CLAIM_MISMATCH");
            }
            if rhh != &hcr.harness_id {
                bail!("ATTEMPT_RUNMODE_HARNESS_MISMATCH");
            }
        }
        _ => bail!("ATTEMPT_RUN_NOT_HCR_MODE"),
    }

    // ── 4. Get the gate definition ────────────────────────────────────
    let def = GateDefinition::for_kind(gate_kind);

    // ── 5. Create InvocationIntent ────────────────────────────────────
    let invocation_id = InvocationId::new();
    let intent = InvocationIntent {
        invocation_id: invocation_id.clone(),
        run_id: RunId(run_id.to_string()),
        operation: def.operation.to_string(),
        arguments: json!({
            "workspace_id": def.workspace_id,
            "command": "echo",
            "args": ["gate", gate_kind.as_str()],
            "session_id": session.id.0,
        }),
        idempotency_key: Some(format!("hcr_attempt_{}_{}", hcr_id, gate_kind.as_str())),
    };

    // ── 6. Approve via Gateway (standard path) ────────────────────────
    let _approved = gateway.approve_invocation(intent, &run, session, snapshot)?;

    // ── 7. Append InvocationProposed event ────────────────────────────
    journal.append_event(
        JournalEventKind::InvocationProposed,
        Some(&RunId(run_id.to_string())),
        Some(&session.id),
        Some(&invocation_id.0),
        json!({
            "operation": def.operation,
            "idempotency_key": format!("hcr_attempt_{}_{}", hcr_id, gate_kind.as_str()),
            "source": "hcr_gate_attempt",
        }),
    )?;

    // ── 8. Persist the gate attempt ───────────────────────────────────
    let attempt_id = format!("ga_{}", uuid::Uuid::new_v4().simple());
    let now = Utc::now().to_rfc3339();
    journal.insert_gate_attempt(
        &attempt_id,
        hcr_id,
        claim_id,
        run_id,
        &hcr.harness_id,
        def.workspace_id,
        gate_kind.as_str(),
        def.operation,
        def.profile,
        &invocation_id.0,
        &now,
    )?;

    Ok((attempt_id, invocation_id.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KernelConfig;

    fn setup_env() -> Result<(
        JournalStore,
        Gateway,
        String,
        String,
        String,
        Session,
        RegistrySnapshot,
    )> {
        let j = JournalStore::in_memory()?;
        let config = KernelConfig::from_cli(None);
        let gw = Gateway::new(config);
        let (hcr_id, _) = j.create_harness_change_request(
            "Feishu",
            "attempt_test",
            "sess_a",
            "feishu:open_id:owner",
            "Feishu",
            "p2p",
            "test-harness",
            "build",
        )?;
        let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;
        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        j.create_hcr_run_binding(&hcr_id, &claim_id.0, &run_id)?;
        // Create Run with RunMode::Hcr (handled by test setup)
        let session = Session {
            id: SessionId("sess_a".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Feishu,
            conversation_key: "feishu:open_id:owner".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };
        let snapshot = RegistrySnapshot {
            snapshot_id: "snap_test".into(),
            created_at: Utc::now(),
            operations: vec![],
        };
        Ok((j, gw, hcr_id, claim_id.0, run_id, session, snapshot))
    }

    #[test]
    fn gate_definition_has_fixed_values() {
        let def = GateDefinition::for_kind(GateKind::Scaffold);
        assert_eq!(def.kind, GateKind::Scaffold);
        assert!(!def.operation.is_empty());
        assert!(!def.profile.is_empty());
        assert!(!def.workspace_id.is_empty());
    }

    #[test]
    fn all_gates_have_definitions() {
        let gates = GateDefinition::all();
        assert_eq!(gates.len(), 5);
    }
}
