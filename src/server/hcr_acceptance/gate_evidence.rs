//! HCR gate attempt and evidence persistence — harness→settlement hotfix.
//!
//! After the Kernel validates a Harness acceptance response (H2), this module
//! maps each of the five structured gate results into:
//!
//! 1. A canonical `HcrGateAttempt` (insert_gate_attempt)
//! 2. An `InvocationProposed` journal event (append_event)
//! 3. A `ReceiptReceived` journal event encoding the gate outcome (append_event)
//! 4. An `HcrGateEvidence` linking the attempt to its receipt (insert_evidence_atomically)
//!
//! The resulting five attempts + five evidence satisfy `settle_hcr_in_tx`'s
//! strong requirement: exactly five gate kinds, each with a matching
//! InvocationProposed→ReceiptReceived chain.
//!
//! Idempotency:
//! - Deterministic IDs derived from (hcr_id, claim_id, run_id, gate_kind)
//!   prevent duplicate gate_attempt rows via UNIQUE(hcr_id, claim_id, run_id, gate_kind).
//! - Existing evidence is detected via `get_gate_evidence_for_hcr` and skipped.
//! - Per-gate events are appended with their own intent_id (UNIQUE constraint).
//! - Crash recovery: partial attempts are detected and completed on retry.

use crate::domain::*;
use crate::hcr::gate_attempt::GateDefinition;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Expected operation for the aggregate harness accept path.
/// All five gate attempts use this operation.
const ACCEPT_OPERATION: &str = "external.coding_hcr_accept";

/// Persist five gate attempts and evidence from an already-validated
/// harness acceptance response.
///
/// `harness_result` must be the `result` object from the harness response
/// (after unwrapping the `external-harness-v1` envelope).  Validation MUST
/// have passed before calling this function.
pub fn persist_gates(
    journal: &JournalStore,
    harness_result: &Value,
    hcr_id: &str,
    claim_id: &str,
    run_id: &str,
    harness_id: &str,
) -> Result<()> {
    // Extract gate_results from the validated harness response.
    let gate_results = match harness_result
        .get("gate_results")
        .and_then(|v| v.as_array())
    {
        Some(arr) if arr.len() == 5 => arr,
        _ => bail!("GATE_RESULTS_MISSING_OR_INCOMPLETE"),
    };

    // Load existing state for idempotency checks.
    let existing_attempts = journal.get_attempts_for_hcr(hcr_id, claim_id, run_id)?;
    let existing_evidence = journal.get_gate_evidence_for_hcr(hcr_id, claim_id, run_id)?;

    let existing_by_kind: std::collections::HashMap<String, &HcrGateAttempt> = existing_attempts
        .iter()
        .map(|a| (a.gate_kind.clone(), a))
        .collect();

    let evidence_attempt_ids: std::collections::HashSet<String> = existing_evidence
        .iter()
        .map(|e| e.gate_attempt_id.clone())
        .collect();

    for gate_val in gate_results {
        let gate_kind = match gate_val.get("gate_kind").and_then(|v| v.as_str()) {
            Some(k) => k.to_string(),
            None => bail!("MISSING_GATE_KIND"),
        };

        // --- Idempotency check 1: attempt + evidence already exist ---
        if let Some(attempt) = existing_by_kind.get(&gate_kind) {
            if evidence_attempt_ids.contains(&attempt.gate_attempt_id) {
                // Fully processed — skip.
                continue;
            }
            // Attempt exists but evidence missing — recreate evidence only.
            // (Caller already validated the response; events are appended below.)
        }

        // Parse gate kind enum for GateDefinition.
        let kind_enum = GateKind::from_str(&gate_kind)
            .ok_or_else(|| anyhow::anyhow!("INVALID_GATE_KIND: {gate_kind}"))?;
        let def = GateDefinition::for_kind(kind_enum);

        // Deterministic identity tied to (hcr_id, claim_id, run_id, gate_kind).
        let gate_uid = uid(hcr_id, claim_id, run_id, &gate_kind);
        let attempt_id = format!("accept_ga_{gate_uid}");
        let intent_id = format!("accept_gate_intent_{gate_uid}");

        // --- Parse gate outcome fields ---
        let passed = gate_val
            .get("passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_candidate_failure = gate_val
            .get("is_candidate_failure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let exit_code = gate_val
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1) as i32;
        let timed_out = gate_val
            .get("timed_out")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let error_code = gate_val
            .get("error_code")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let stdout = gate_val
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let stderr = gate_val
            .get("stderr")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let now = Utc::now().to_rfc3339();

        // Insert attempt if it doesn't already exist.
        // Uses the UNIQUE(hcr_id, claim_id, run_id, gate_kind) constraint
        // to prevent duplicates.
        if !existing_by_kind.contains_key(&gate_kind) {
            journal.insert_gate_attempt(
                &attempt_id,
                hcr_id,
                claim_id,
                run_id,
                harness_id,
                def.workspace_id,
                &gate_kind,
                ACCEPT_OPERATION,
                def.profile,
                &intent_id,
                &now,
            )?;

            // Append InvocationProposed event only for newly created attempts.
            journal.append_event(
                JournalEventKind::InvocationProposed,
                Some(&RunId(run_id.to_string())),
                None,
                Some(&intent_id),
                serde_json::json!({
                    "operation": ACCEPT_OPERATION,
                    "source": "hcr_gate_attempt",
                    "gate_kind": gate_kind,
                }),
            )?;
        } else {
            // Attempt from a prior retry may lack InvocationProposed /
            // ReceiptReceived events (crash after attempt INSERT but before
            // events). We always append a ReceiptReceived below; for the
            // InvocationProposed, we rely on the UNIQUE
            // (invocation_intent_id) constraint on the attempt table.
            journal.append_event(
                JournalEventKind::InvocationProposed,
                Some(&RunId(run_id.to_string())),
                None,
                Some(&intent_id),
                serde_json::json!({
                    "operation": ACCEPT_OPERATION,
                    "source": "hcr_gate_attempt",
                    "gate_kind": gate_kind,
                }),
            )?;
        }

        // Append ReceiptReceived event with gate outcome.
        let receipt_status = if passed { "Succeeded" } else { "Failed" };
        let child_cleanup = !timed_out;
        let receipt_payload = serde_json::json!({
            "status": receipt_status,
            "output": {
                "exit_code": exit_code,
                "timed_out": timed_out,
                "child_cleanup": child_cleanup,
                "error_category": error_code,
                "stdout": stdout,
                "stderr": stderr,
            }
        });

        let receipt_event = journal.append_event(
            JournalEventKind::ReceiptReceived,
            Some(&RunId(run_id.to_string())),
            None,
            Some(&intent_id),
            receipt_payload.clone(),
        )?;

        // Compute payload digest (matches settle_hcr_in_tx algorithm).
        let payload_digest = {
            let mut hasher = Sha256::new();
            hasher.update(format!("{:?}", receipt_payload).as_bytes());
            format!("sha256:{}", hex::encode(hasher.finalize()))
        };

        // Insert evidence (idempotent: skips if attempt_id already has evidence).
        let evidence_id = format!("ev_accept_{gate_uid}");
        match journal.insert_evidence_atomically(
            &evidence_id,
            &attempt_id,
            &receipt_event.event_id.0,
            &payload_digest,
            &now,
        ) {
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("EVIDENCE_INSERT_FAILED") || msg.contains("UNIQUE") {
                    // Already exists — idempotent skip.
                    continue;
                }
                bail!("EVIDENCE_PERSIST_FAILED_{gate_kind}: {msg}");
            }
        }
    }

    Ok(())
}

/// Deterministic SHA-256 prefix for (hcr, claim, run, gate_kind).
/// Collision-resistant: same identity tuple → same UID.
fn uid(hcr_id: &str, claim_id: &str, run_id: &str, gate_kind: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hcr_id.as_bytes());
    hasher.update(b"|");
    hasher.update(claim_id.as_bytes());
    hasher.update(b"|");
    hasher.update(run_id.as_bytes());
    hasher.update(b"|");
    hasher.update(gate_kind.as_bytes());
    hex::encode(hasher.finalize())[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hcr::settlement::settle_hcr;
    use crate::journal::JournalStore;
    use chrono::Utc;
    use serde_json::json;

    // ── Helpers ──

    fn setup_fixture() -> (JournalStore, String, String, String, String) {
        let j = JournalStore::in_memory().unwrap();
        let (hcr_id, _) = j
            .create_harness_change_request(
                "Feishu",
                "gate_ev_test",
                "s_gev",
                "feishu:open_id:owner",
                "Feishu",
                "p2p",
                "test-harness",
                "build",
            )
            .unwrap();
        let claim_id = j
            .claim_hcr_for_execution(&hcr_id, "test-harness", "w1")
            .unwrap()
            .0;
        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        j.create_hcr_run_binding(&hcr_id, &claim_id, &run_id)
            .unwrap();
        let run = Run {
            id: RunId(run_id.clone()),
            session_id: SessionId("s_gev".into()),
            agent_id: AgentId("main".into()),
            trigger_event_id: EventId::new(),
            principal: RunPrincipal {
                principal_id: PrincipalId("feishu:open_id:owner".into()),
                subject: PrincipalSubject::FeishuOpenId("feishu:open_id:owner".into()),
                source: PrincipalSource::Feishu,
                grants: vec![],
                requester_id: None,
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            registry_snapshot_id: "s".into(),
            mode: RunMode::Hcr {
                hcr_id: hcr_id.clone(),
                harness_id: "test-harness".into(),
                claim_id: claim_id.clone(),
            },
        };
        j.insert_run(&run).unwrap();
        (j, hcr_id, claim_id, run_id, "test-harness".to_string())
    }

    fn all_pass_gates() -> Vec<Value> {
        let kinds = [
            "scaffold",
            "build",
            "trusted_test",
            "trusted_smoke",
            "artifact",
        ];
        kinds
            .iter()
            .map(|k| {
                json!({
                    "gate_kind": k,
                    "passed": true,
                    "is_candidate_failure": false,
                    "exit_code": 0,
                    "timed_out": false,
                    "error_code": null,
                    "stdout": "",
                    "stderr": "",
                })
            })
            .collect()
    }

    fn harness_result(gates: Vec<Value>, outcome: &str) -> Value {
        json!({
            "overall_outcome": outcome,
            "gate_results": gates,
        })
    }

    // ── Test 1: Positive - 5 gates → 5 attempts → 5 evidence → settlement ──

    #[test]
    fn persist_five_gates_and_settlement_succeeds() {
        let (j, hcr_id, claim_id, run_id, harness_id) = setup_fixture();
        let result = harness_result(all_pass_gates(), "CandidatePassed");

        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        let attempts = j.get_attempts_for_hcr(&hcr_id, &claim_id, &run_id).unwrap();
        assert_eq!(attempts.len(), 5, "attempt count");

        let evidence = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(evidence.len(), 5, "evidence count");

        let kinds: Vec<&str> = attempts.iter().map(|a| a.gate_kind.as_str()).collect();
        for k in &[
            "scaffold",
            "build",
            "trusted_test",
            "trusted_smoke",
            "artifact",
        ] {
            assert!(kinds.contains(k), "missing gate {k}");
        }

        let settlement = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(
            matches!(&settlement, SettlementResult::Succeeded(_)),
            "expected Succeeded, got {:?}",
            settlement
        );
    }

    // ── Test 2: Replay - same persist_gates call is idempotent ──

    #[test]
    fn replay_persist_gates_is_idempotent() {
        let (j, hcr_id, claim_id, run_id, harness_id) = setup_fixture();
        let result = harness_result(all_pass_gates(), "CandidatePassed");

        // First call
        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        // Replay
        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        let attempts = j.get_attempts_for_hcr(&hcr_id, &claim_id, &run_id).unwrap();
        assert_eq!(attempts.len(), 5, "attempts after replay = 5");

        let evidence = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(evidence.len(), 5, "evidence after replay = 5");

        // Settlement must succeed after replay
        let settlement = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(
            matches!(&settlement, SettlementResult::Succeeded(_)),
            "expected Succeeded after replay, got {:?}",
            settlement
        );
    }

    // ── Test 3: CandidateFailed ──

    #[test]
    fn candidate_failure_produces_5_evidence_and_candidate_failed_settlement() {
        let (j, hcr_id, claim_id, run_id, harness_id) = setup_fixture();

        let mut gates = all_pass_gates();
        gates[3] = json!({
            "gate_kind": "trusted_smoke",
            "passed": false,
            "is_candidate_failure": true,
            "exit_code": 1,
            "timed_out": false,
            "error_code": null,
            "stdout": "",
            "stderr": "multiply(6,7) returned 41",
        });
        let result = harness_result(gates, "CandidateFailed");

        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        let attempts = j.get_attempts_for_hcr(&hcr_id, &claim_id, &run_id).unwrap();
        assert_eq!(attempts.len(), 5, "attempt count for candidate_failed");

        let evidence = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(evidence.len(), 5, "evidence count for candidate_failed");

        let settlement = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(
            matches!(&settlement, SettlementResult::CandidateFailed(_)),
            "expected CandidateFailed, got {:?}",
            settlement
        );
    }

    // ── Test 4: InfrastructureFailure - harness unreachable → no gate processing ──

    #[test]
    fn harness_unreachable_creates_no_gate_state() {
        let (j, hcr_id, claim_id, run_id, _harness_id) = setup_fixture();
        // No persist_gates call — simulates a failed accept handler

        let attempts = j.get_attempts_for_hcr(&hcr_id, &claim_id, &run_id).unwrap();
        assert_eq!(attempts.len(), 0, "no attempts for infra failure");

        let evidence = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(evidence.len(), 0, "no evidence for infra failure");

        let settlement = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(
            matches!(&settlement, SettlementResult::EvidenceIncomplete(_)),
            "expected EvidenceIncomplete, got {:?}",
            settlement
        );
    }

    // ── Test 5: Forged response - validation rejects it ──

    #[test]
    fn forged_wrong_hcr_id_rejected_by_validation() {
        use crate::server::hcr_acceptance::response_validation::{
            validate_harness_response, RequestContext,
        };

        let ctx = RequestContext {
            hcr_id: "real_hcr".into(),
            claim_id: "real_claim".into(),
            run_id: "real_run".into(),
            principal_id: "real_principal".into(),
            gateway_session_id: "real_session".into(),
            registry_snapshot_id: "real_snapshot".into(),
            operation: "external.coding_hcr_accept".into(),
            idempotency_key: "real_key".into(),
        };
        let forged = json!({
            "protocol_version": "external-harness-v1",
            "result": {
                "hcr_id": "wrong_hcr",
                "claim_id": "real_claim",
                "run_id": "real_run",
                "principal_id": "real_principal",
                "gateway_session_id": "real_session",
                "registry_snapshot_id": "real_snapshot",
                "operation": "external.coding_hcr_accept",
                "idempotency_key": "real_key",
                "harness_execution_id": "he_123",
                "candidate_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "evidence_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "overall_outcome": "CandidatePassed",
                "gate_results": [
                    {"gate_kind":"scaffold","passed":true,"is_candidate_failure":false,"exit_code":0,"timed_out":false,"error_code":null,"stdout":"","stderr":""},
                    {"gate_kind":"build","passed":true,"is_candidate_failure":false,"exit_code":0,"timed_out":false,"error_code":null,"stdout":"","stderr":""},
                    {"gate_kind":"trusted_test","passed":true,"is_candidate_failure":false,"exit_code":0,"timed_out":false,"error_code":null,"stdout":"","stderr":""},
                    {"gate_kind":"trusted_smoke","passed":true,"is_candidate_failure":false,"exit_code":0,"timed_out":false,"error_code":null,"stdout":"","stderr":""},
                    {"gate_kind":"artifact","passed":true,"is_candidate_failure":false,"exit_code":0,"timed_out":false,"error_code":null,"stdout":"","stderr":""}
                ]
            }
        });
        let result = validate_harness_response(&forged, &ctx);
        assert!(result.is_err(), "forged hcr_id should be rejected");
        assert!(
            result.unwrap_err().contains("hcr_id"),
            "error should mention hcr_id"
        );
    }

    #[test]
    fn forged_missing_gate_results_rejected() {
        use crate::server::hcr_acceptance::response_validation::{
            validate_harness_response, RequestContext,
        };

        let ctx = RequestContext {
            hcr_id: "hcr_1".into(),
            claim_id: "cl_1".into(),
            run_id: "rn_1".into(),
            principal_id: "p_1".into(),
            gateway_session_id: "gs_1".into(),
            registry_snapshot_id: "rs_1".into(),
            operation: "external.coding_hcr_accept".into(),
            idempotency_key: "ik_1".into(),
        };
        let forged = json!({
            "protocol_version": "external-harness-v1",
            "result": {
                "hcr_id": "hcr_1",
                "claim_id": "cl_1",
                "run_id": "rn_1",
                "principal_id": "p_1",
                "gateway_session_id": "gs_1",
                "registry_snapshot_id": "rs_1",
                "operation": "external.coding_hcr_accept",
                "idempotency_key": "ik_1",
                "harness_execution_id": "he_1",
                "candidate_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "evidence_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "overall_outcome": "CandidatePassed",
                "gate_results": [
                    {"gate_kind":"scaffold","passed":true,"is_candidate_failure":false,"exit_code":0,"timed_out":false,"error_code":null,"stdout":"","stderr":""},
                    {"gate_kind":"build","passed":true,"is_candidate_failure":false,"exit_code":0,"timed_out":false,"error_code":null,"stdout":"","stderr":""}
                ]
            }
        });
        let result = validate_harness_response(&forged, &ctx);
        assert!(result.is_err(), "expected 5 gates, got 2");
    }

    // ── Test 6: Partial-write recovery - 2 gates persisted, retry completes to 5 ──

    #[test]
    fn partial_write_recovery_completes_to_five_gates() {
        let (j, hcr_id, claim_id, run_id, harness_id) = setup_fixture();
        let result = harness_result(all_pass_gates(), "CandidatePassed");

        // Simulate partial write: manually insert attempts for first 2 gates
        let all_attempts = result
            .get("gate_results")
            .and_then(|v| v.as_array())
            .unwrap();
        for gate_val in &all_attempts[..2] {
            let gk = gate_val.get("gate_kind").and_then(|v| v.as_str()).unwrap();
            let kind_enum = GateKind::from_str(gk).unwrap();
            let def = GateDefinition::for_kind(kind_enum);
            let g_uid = uid(&hcr_id, &claim_id, &run_id, gk);
            j.insert_gate_attempt(
                &format!("accept_ga_{g_uid}"),
                &hcr_id,
                &claim_id,
                &run_id,
                &harness_id,
                def.workspace_id,
                gk,
                ACCEPT_OPERATION,
                def.profile,
                &format!("accept_gate_intent_{g_uid}"),
                &Utc::now().to_rfc3339(),
            )
            .unwrap();
        }

        // Now run the full persist — should detect missing evidence and complete.
        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        let attempts = j.get_attempts_for_hcr(&hcr_id, &claim_id, &run_id).unwrap();
        assert_eq!(attempts.len(), 5, "attempts after partial recovery");

        let evidence = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(evidence.len(), 5, "evidence after partial recovery");

        let settlement = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(
            matches!(&settlement, SettlementResult::Succeeded(_)),
            "expected Succeeded after partial recovery, got {:?}",
            settlement
        );
    }

    // ── Test 7: Duplicate settlement after full persist is idempotent ──

    #[test]
    fn duplicate_settle_after_full_persist_is_idempotent() {
        let (j, hcr_id, claim_id, run_id, harness_id) = setup_fixture();
        let result = harness_result(all_pass_gates(), "CandidatePassed");

        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        // First settlement
        let s1 = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(matches!(&s1, SettlementResult::Succeeded(_)));

        // Second settlement must be idempotent
        let s2 = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(
            matches!(&s2, SettlementResult::AlreadySettled(_)),
            "expected AlreadySettled, got {:?}",
            s2
        );
    }

    // ── Test 8: Partial evidence — manually delete 2 evidence, persist_gates recovers ──

    #[test]
    fn partial_evidence_recovery_completes_to_five() {
        let (j, hcr_id, claim_id, run_id, harness_id) = setup_fixture();
        let result = harness_result(all_pass_gates(), "CandidatePassed");

        // First run: full persist creates 5 attempts + 5 evidence.
        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        let before = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(before.len(), 5, "initial evidence count");

        // Manually delete 2 evidence rows to simulate partial evidence loss.
        let attempt_ids: Vec<String> = j
            .get_attempts_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap()
            .iter()
            .map(|a| a.gate_attempt_id.clone())
            .collect();
        {
            let conn = j.conn.lock().unwrap();
            // Delete evidence for last 2 gates by matching attempt_id pattern
            for aid in &attempt_ids[3..] {
                conn.execute(
                    "DELETE FROM hcr_gate_evidence WHERE gate_attempt_id = ?1",
                    rusqlite::params![aid],
                )
                .unwrap();
            }
        }

        let after_delete = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(after_delete.len(), 3, "evidence count after delete");

        // Re-run persist_gates — should detect missing evidence and recreate.
        persist_gates(&j, &result, &hcr_id, &claim_id, &run_id, &harness_id).unwrap();

        let recovered = j
            .get_gate_evidence_for_hcr(&hcr_id, &claim_id, &run_id)
            .unwrap();
        assert_eq!(recovered.len(), 5, "evidence count after recovery");

        // Settlement must succeed after recovery.
        let settlement = settle_hcr(&j, &hcr_id, &claim_id, &run_id).unwrap();
        assert!(
            matches!(&settlement, SettlementResult::Succeeded(_)),
            "expected Succeeded after evidence recovery, got {:?}",
            settlement
        );
    }
}
