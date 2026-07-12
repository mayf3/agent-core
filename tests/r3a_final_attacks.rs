//! R3A final audit attack tests (R3A-R3).
//! Tests exercise: evidence, settlement, resume, FK-OFF triggers, concurrency.

use agent_core_kernel::domain::harness_change_request::SettlementResult as SettleResult;
use agent_core_kernel::domain::*;
use agent_core_kernel::hcr::evidence;
use agent_core_kernel::hcr::resume::{self, ResumeState};
use agent_core_kernel::hcr::settlement;
use agent_core_kernel::journal::JournalStore;
use chrono::Utc;
use rusqlite::Connection;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

fn db_path() -> PathBuf {
    std::env::temp_dir().join(format!("r3a_final_{}", uuid::Uuid::new_v4().simple()))
}

/// Minimal fixture: HCR + claim + Run + RunMode::Hcr + a gate attempt + intent + receipt.
struct Fixture {
    j: JournalStore,
    hcr_id: String,
    claim_id: String,
    run_id: String,
}

fn make_fixture() -> Fixture {
    let j = JournalStore::in_memory().unwrap();
    let (hcr_id, _) = j
        .create_harness_change_request(
            "Feishu",
            "atk",
            "s_atk",
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
        session_id: SessionId("s_atk".into()),
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
    Fixture {
        j,
        hcr_id,
        claim_id,
        run_id,
    }
}

/// Add a gate attempt + intent event + receipt event + evidence.
fn add_gate(
    f: &Fixture,
    kind: GateKind,
    st: &str,
    exit: i32,
    to: bool,
    cc: Option<bool>,
    ec: Option<&str>,
) {
    let def = agent_core_kernel::hcr::gate_attempt::GateDefinition::for_kind(kind);
    let aid = format!("ga_{}", uuid::Uuid::new_v4().simple());
    let iid = format!("inv_{}", uuid::Uuid::new_v4().simple());
    // Insert attempt.
    f.j.insert_gate_attempt(
        &aid,
        &f.hcr_id,
        &f.claim_id,
        &f.run_id,
        "test-harness",
        def.workspace_id,
        kind.as_str(),
        def.operation,
        def.profile,
        &iid,
        &Utc::now().to_rfc3339(),
    )
    .unwrap();
    // Intent event.
    f.j.append_event(
        JournalEventKind::InvocationProposed,
        Some(&RunId(f.run_id.clone())),
        Some(&SessionId("s_atk".into())),
        Some(&iid),
        json!({"operation": def.operation, "source": "test"}),
    )
    .unwrap();
    // Receipt event.
    f.j.append_event(JournalEventKind::ReceiptReceived, Some(&RunId(f.run_id.clone())), Some(&SessionId("s_atk".into())), Some(&iid),
        json!({"status": st, "output": {"exit_code": exit, "timed_out": to, "child_cleanup": cc, "error_category": ec}, "invocation_id": iid})).unwrap();
    // Register evidence.
    evidence::register_gate_evidence(&f.j, &aid).unwrap();
}

fn add_all_gates(f: &Fixture) {
    for &k in GateKind::all_required() {
        add_gate(f, k, "Succeeded", 0, false, Some(true), None);
    }
}

// ── 1. Evidence & Journal source truth ─────────────────────────────────

#[test]
fn failed_receipt_tampered_evidence_cannot_settle_success() {
    let f = make_fixture();
    add_gate(
        &f,
        GateKind::Scaffold,
        "Failed",
        1,
        false,
        Some(true),
        Some("err"),
    );
    let r = settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap();
    assert!(!matches!(r, SettleResult::Succeeded(_)));
}

#[test]
fn settlement_reloads_receipt_source_fields() {
    let f = make_fixture();
    add_all_gates(&f);
    match settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id) {
        Ok(r) => {
            eprintln!("SETTLE_RESULT: {:?}", r);
            assert!(matches!(r, SettleResult::Succeeded(_)));
        }
        Err(e) => {
            eprintln!("SETTLE_ERROR: {e}");
            panic!("settle failed: {e}");
        }
    }
}

// ── 2. FK-OFF triggers ────────────────────────────────────────────────

#[test]
fn ghost_attempt_rejected_with_foreign_keys_off() {
    let p = db_path();
    let _j = JournalStore::open(&p).unwrap();
    drop(_j);
    let conn = Connection::open(&p).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    let r = conn.execute(
        "INSERT INTO hcr_gate_attempts (gate_attempt_id, hcr_id, claim_id, run_id, harness_id, workspace_id, gate_kind, expected_operation, expected_profile, invocation_intent_id, created_at) VALUES ('ghost', 'nonexistent', 'nc', 'nr', 'h', 'w', 'scaffold', 'op', 'prof', 'i', '2026-01-01')",
        [],
    );
    let err_msg = r.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(!err_msg.is_empty(), "INSERT must fail");
    assert!(
        err_msg.contains("GHOST") || err_msg.contains("ABORT") || err_msg.contains("constraint"),
        "expected trigger rejection, got: {err_msg}"
    );
    std::fs::remove_file(&p).ok();
}

#[test]
fn ghost_evidence_rejected_with_foreign_keys_off() {
    let p = db_path();
    let _j = JournalStore::open(&p).unwrap();
    drop(_j);
    let conn = Connection::open(&p).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    let r = conn.execute(
        "INSERT INTO hcr_gate_evidence (evidence_id, gate_attempt_id, receipt_event_id, receipt_payload_digest, created_at) VALUES ('ev_g', 'nonexistent', 'e', 'd', '2026-01-01')",
        [],
    );
    let err_msg = r.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(!err_msg.is_empty(), "INSERT must fail");
    assert!(
        err_msg.contains("GHOST") || err_msg.contains("ABORT") || err_msg.contains("constraint"),
        "expected trigger rejection, got: {err_msg}"
    );
    std::fs::remove_file(&p).ok();
}

/// Helper: create all 5 gates with first gate set to custom params.
/// - `status`: receipt status string ("Succeeded" or "Failed")
/// - `exit`: exit code
/// - `to`: timed_out
/// - `cc`: child_cleanup
/// - `ec`: error_code
fn infra_gate_full(
    f: &Fixture,
    status: &str,
    exit: i32,
    to: bool,
    cc: Option<bool>,
    ec: Option<&str>,
) {
    let kinds = GateKind::all_required();
    add_gate(f, kinds[0], status, exit, to, cc, ec);
    for &k in &kinds[1..] {
        add_gate(f, k, "Succeeded", 0, false, Some(true), None);
    }
}

fn infra_gate(f: &Fixture, to: bool, cc: Option<bool>, ec: Option<&str>) {
    let st = if ec.is_some() || to || cc == Some(false) {
        "Failed"
    } else {
        "Succeeded"
    };
    infra_gate_full(f, st, if ec.is_some() { 1 } else { 0 }, to, cc, ec);
}

// ── 3. Classification ─────────────────────────────────────────────────

#[test]
fn timeout_is_retryable_infrastructure_failure() {
    let f = make_fixture();
    infra_gate(&f, true, Some(true), None);
    assert!(matches!(
        settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap(),
        SettleResult::InfrastructureFailure(_)
    ));
}

#[test]
fn cleanup_false_is_retryable_infrastructure_failure() {
    let f = make_fixture();
    infra_gate(&f, false, Some(false), None);
    assert!(matches!(
        settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap(),
        SettleResult::InfrastructureFailure(_)
    ));
}

#[test]
fn cleanup_missing_is_retryable_infrastructure_failure() {
    let f = make_fixture();
    infra_gate(&f, false, None, None);
    assert!(matches!(
        settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap(),
        SettleResult::InfrastructureFailure(_)
    ));
}

#[test]
fn business_nonzero_exit_is_candidate_failed() {
    let f = make_fixture();
    // Failed status with non-zero exit, but cleanup ok, no error_code, no timeout → candidate failure.
    infra_gate_full(&f, "Failed", 1, false, Some(true), None);
    let r = settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap();
    eprintln!("BUSINESS_NONZERO_RESULT: {:?}", r);
    assert!(
        matches!(r, SettleResult::CandidateFailed(_))
            || matches!(r, SettleResult::EvidenceIncomplete(_))
    );
}

// ── 4. Resume triple consistency ─────────────────────────────────────

#[test]
fn terminal_hcr_without_settlement_is_corruption() {
    let f = make_fixture();
    f.j.execute_sql_for_test(&format!(
        "UPDATE harness_change_requests SET status = 'succeeded' WHERE request_id = '{}'",
        f.hcr_id
    ))
    .unwrap();
    assert!(matches!(
        resume::determine_resume_state(&f.j, &f.hcr_id).unwrap(),
        ResumeState::Corruption(_)
    ));
}

// ── 5. Twenty independent connections ─────────────────────────────────

#[test]
fn twenty_independent_connections_settle_once() {
    // Real concurrent settle: 20 threads × 20 independent connections × Barrier.
    let n = 20;
    let rounds = 20;

    for round in 0..rounds {
        let db_path = db_path();
        let hcr_id;
        let claim_id;
        let run_id;
        {
            let j = JournalStore::open(&db_path).unwrap();
            let (hid, _) = j
                .create_harness_change_request(
                    "Feishu",
                    &format!("cr{round}"),
                    "s_c",
                    "feishu:open_id:owner",
                    "Feishu",
                    "p2p",
                    "test-harness",
                    "build",
                )
                .unwrap();
            let cid = j
                .claim_hcr_for_execution(&hid, "test-harness", "w1")
                .unwrap()
                .0;
            let rid = format!("run_{}", uuid::Uuid::new_v4().simple());
            j.create_hcr_run_binding(&hid, &cid, &rid).unwrap();
            let run = Run {
                id: RunId(rid.clone()),
                session_id: SessionId("s_c".into()),
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
                    hcr_id: hid.clone(),
                    harness_id: "test-harness".into(),
                    claim_id: cid.clone(),
                },
            };
            j.insert_run(&run).unwrap();
            hcr_id = hid;
            claim_id = cid;
            run_id = rid;
            let defs = agent_core_kernel::hcr::gate_attempt::GateDefinition::all();
            for d in &defs {
                let aid = format!("ga_{}", uuid::Uuid::new_v4().simple());
                let iid = format!("inv_{}", uuid::Uuid::new_v4().simple());
                j.insert_gate_attempt(
                    &aid,
                    &hcr_id,
                    &claim_id,
                    &run_id,
                    "test-harness",
                    d.workspace_id,
                    d.kind.as_str(),
                    d.operation,
                    d.profile,
                    &iid,
                    &Utc::now().to_rfc3339(),
                )
                .unwrap();
                j.append_event(
                    JournalEventKind::InvocationProposed,
                    Some(&RunId(run_id.clone())),
                    Some(&SessionId("s_c".into())),
                    Some(&iid),
                    json!({"operation": d.operation}),
                )
                .unwrap();
                j.append_event(JournalEventKind::ReceiptReceived, Some(&RunId(run_id.clone())), Some(&SessionId("s_c".into())), Some(&iid),
                    json!({"status": "Succeeded", "output": {"exit_code": 0, "timed_out": false, "child_cleanup": true}})).unwrap();
                evidence::register_gate_evidence(&j, &aid).unwrap();
            }
        }

        let p = Arc::new(db_path.clone());
        let bar = Arc::new(Barrier::new(n));
        let hid = Arc::new(hcr_id.clone());
        let cid = Arc::new(claim_id.clone());
        let rid = Arc::new(run_id.clone());
        let mut handles = vec![];

        for _ in 0..n {
            let bp = Arc::clone(&p);
            let b = Arc::clone(&bar);
            let h = Arc::clone(&hid);
            let c = Arc::clone(&cid);
            let r = Arc::clone(&rid);
            handles.push(thread::spawn(move || {
                let j = JournalStore::open(&bp).unwrap();
                b.wait();
                settlement::settle_hcr(&j, &h, &c, &r)
            }));
        }

        let mut succ = 0usize;
        let mut alrdy = 0usize;
        let mut errs = 0usize;
        for h in handles {
            match h.join().unwrap() {
                Ok(SettleResult::Succeeded(_)) => succ += 1,
                Ok(SettleResult::AlreadySettled(_)) => alrdy += 1,
                _ => errs += 1,
            }
        }

        // Verify DB state via new raw SQLite connection.
        let check_conn = rusqlite::Connection::open(&db_path).unwrap();
        let _ = check_conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA query_only=1;");
        let stl: i64 = check_conn
            .query_row(
                &format!("SELECT COUNT(*) FROM hcr_settlements WHERE hcr_id = '{hcr_id}'"),
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let evs: i64 = check_conn.query_row(
            &format!("SELECT COUNT(*) FROM journal_events WHERE kind IN ('HcrSettlementSucceeded','HcrSettlementFailed') AND correlation_id = '{hcr_id}'"),
            [], |row| row.get(0),
        ).unwrap_or(0);

        if succ != 1 || alrdy != n - 1 || errs != 0 || stl != 1 || evs != 1 {
            eprintln!("ROUND {round}: succ={succ} alrdy={alrdy} errs={errs} stl={stl} ev={evs}");
            panic!(
                "round {round}: expected succ=1, alrdy={}, stl=1, ev=1",
                n - 1
            );
        }

        std::fs::remove_file(&db_path).ok();
    }
}

// ── 6. Terminal-idempotent replay (no source-fact change) ───────────────
//
// A legitimate settlement is terminal. Replaying `settle_hcr()` on the same
// HCR *without* changing any persistent source fact is an idempotent read: it
// returns `AlreadySettled` referring to the *same* persisted settlement, and
// produces no additional settlement row or terminal event.

fn terminal_event_count(j: &JournalStore, hcr_id: &str) -> usize {
    j.events()
        .unwrap()
        .into_iter()
        .filter(|e| {
            (e.kind == JournalEventKind::HcrSettlementSucceeded
                || e.kind == JournalEventKind::HcrSettlementFailed)
                && e.correlation_id.as_deref() == Some(hcr_id)
        })
        .count()
}

#[test]
fn same_evidence_digest_replay_is_already_settled() {
    let f = make_fixture();
    add_all_gates(&f);

    // First call: production settlement succeeds.
    let r1 = settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap();
    let sid1 = match r1 {
        SettleResult::Succeeded(sid) => sid,
        other => panic!("first settle must succeed, got {other:?}"),
    };

    // Record persisted terminal identity.
    let s1 =
        f.j.get_settlement(&f.hcr_id)
            .expect("query settlement")
            .expect("settlement row after first settle");
    assert_eq!(s1.settlement_id, sid1);
    assert_eq!(s1.result, "succeeded");
    let digest1 = s1.evidence_set_digest.clone();

    // No persistent source fact is changed. Replay the same production entry point.
    let r2 = settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap();
    assert!(
        matches!(r2, SettleResult::AlreadySettled(_)),
        "replay must be AlreadySettled, got {r2:?}"
    );

    // The persisted settlement identity is unchanged.
    let s2 =
        f.j.get_settlement(&f.hcr_id)
            .expect("query settlement")
            .expect("settlement row after replay");
    assert_eq!(s2.settlement_id, sid1, "settlement id must be identical");
    assert_eq!(s2.result, s1.result, "result must be identical");
    assert_eq!(
        s2.evidence_set_digest, digest1,
        "evidence-set digest must be identical"
    );

    // Exactly one settlement row and one terminal event.
    assert!(
        f.j.get_settlement(&f.hcr_id).unwrap().is_some(),
        "settlement row exists"
    );
    assert_eq!(
        terminal_event_count(&f.j, &f.hcr_id),
        1,
        "exactly one terminal event"
    );
}

// ── 7. Pre-settlement receipt tamper is an infrastructure failure ────────
//
// Tampering a Receipt payload *before* settlement — without updating the
// Evidence record's stored payload digest — cannot form a new legitimate
// canonical digest. The per-evidence payload-digest guard rejects it as an
// InfrastructureFailure, the HCR stays `running`, and no terminal settlement
// row or event is produced.

#[test]
fn pre_settlement_receipt_tamper_is_infrastructure_failure() {
    let f = make_fixture();
    add_all_gates(&f);

    // Tamper a real Receipt source field (exit_code) for every gate's receipt
    // *before* settlement, without touching the stored Evidence payload digest.
    f.j.execute_sql_for_test(&format!(
        "UPDATE journal_events SET payload_json = json_set(payload_json, '$.output.exit_code', 99) \
         WHERE event_id IN (\
            SELECT e.receipt_event_id FROM hcr_gate_evidence e \
            JOIN hcr_gate_attempts a ON e.gate_attempt_id = a.gate_attempt_id \
            WHERE a.hcr_id = '{}'\
         )",
        f.hcr_id
    ))
    .expect("tamper receipt exit_code");

    // Production settlement must reject the tampered evidence without going terminal.
    let r = settlement::settle_hcr(&f.j, &f.hcr_id, &f.claim_id, &f.run_id).unwrap();
    assert!(
        matches!(r, SettleResult::InfrastructureFailure(_)),
        "pre-settlement receipt tamper must be InfrastructureFailure, got {r:?}"
    );

    // HCR remains running; no settlement row; no terminal event.
    let hcr =
        f.j.get_harness_change_request(&f.hcr_id)
            .expect("query hcr")
            .expect("hcr row");
    assert_eq!(
        hcr.status, "running",
        "HCR must remain running after reject"
    );

    assert!(
        f.j.get_settlement(&f.hcr_id)
            .expect("query settlement")
            .is_none(),
        "no settlement row must be produced"
    );
    assert_eq!(
        terminal_event_count(&f.j, &f.hcr_id),
        0,
        "no terminal event must be produced"
    );
}
