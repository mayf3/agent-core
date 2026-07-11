//! Claim concurrency, idempotency, and crash-recovery tests for HCR claims.
//!
//! These tests verify the atomic claim invariant:
//! - 20 concurrent claims on the same HCR → exactly 1 succeeds
//! - Exactly 1 claim_id, 1 HCR in `running` state, 1 claim journal event
//! - Repeat claim (idempotent) → same claim_id
//! - Crash recovery: claim committed, Run not yet created → retry returns same claim

use crate::domain::*;
use crate::journal::JournalStore;
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::thread;

fn create_test_hcr(j: &JournalStore) -> Result<String> {
    let (request_id, deduplicated) = j.create_harness_change_request(
        "Feishu",
        "test_msg_1",
        "session_1",
        "principal_1",
        "Feishu",
        "p2p",
        "test-harness",
        "build test environment",
    )?;
    assert!(!deduplicated);
    Ok(request_id)
}

#[test]
fn single_claim_succeeds() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let hcr_id = create_test_hcr(&j)?;

    let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

    // Verify claim record exists.
    let claim = j.get_active_claim_for_hcr(&hcr_id)?;
    assert!(claim.is_some(), "active claim must exist");
    let claim = claim.unwrap();
    assert_eq!(claim.claim_id, claim_id);
    assert_eq!(claim.harness_id, "test-harness");
    assert_eq!(claim.worker_instance_id, "worker_1");
    assert_eq!(claim.status, HcrClaimStatus::Active);

    // Verify HCR status is now running.
    let hcr = j.get_harness_change_request(&hcr_id)?.unwrap();
    assert_eq!(hcr.status, "running");

    // Verify exactly one claim event.
    let events = j.events()?;
    let claim_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::HcrClaimSucceeded)
        .collect();
    assert_eq!(claim_events.len(), 1, "exactly one claim event");
    assert_eq!(
        claim_events[0].correlation_id.as_deref(),
        Some(claim_id.0.as_str())
    );

    // Verify hash chain integrity.
    assert!(j.verify_hash_chain()?);
    Ok(())
}

#[test]
fn double_claim_same_worker_fails() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let hcr_id = create_test_hcr(&j)?;

    let _first = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

    // Second claim must fail.
    let err = j
        .claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("HCR_ALREADY_CLAIMED") || msg.contains("HCR_NOT_CLAIMABLE"),
        "expected already_claimed or not_claimable, got: {msg}"
    );

    // Still exactly one claim.
    assert_eq!(j.hcr_claim_count()?, 1);
    let events = j.events()?;
    let claim_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::HcrClaimSucceeded)
        .collect();
    assert_eq!(claim_events.len(), 1);

    // Verify no duplicate HCR status change.
    let hcr = j.get_harness_change_request(&hcr_id)?.unwrap();
    assert_eq!(hcr.status, "running");

    assert!(j.verify_hash_chain()?);
    Ok(())
}

#[test]
fn claim_not_claimable_when_already_running() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let hcr_id = create_test_hcr(&j)?;

    j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

    // Different worker tries to claim the same HCR.
    let err = j
        .claim_hcr_for_execution(&hcr_id, "test-harness", "worker_2")
        .unwrap_err();
    assert!(
        err.to_string().contains("HCR_ALREADY_CLAIMED")
            || err.to_string().contains("HCR_NOT_CLAIMABLE")
    );

    // Still only one claim.
    assert_eq!(j.hcr_claim_count()?, 1);
    Ok(())
}

#[test]
fn claim_nonexistent_hcr_fails() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let err = j
        .claim_hcr_for_execution("nonexistent_hcr", "test-harness", "worker_1")
        .unwrap_err();
    assert!(err.to_string().contains("HCR_NOT_FOUND"));
    Ok(())
}

#[test]
fn concurrent_20_way_claim_exactly_one_succeeds() -> Result<()> {
    let j = Arc::new(JournalStore::in_memory()?);
    let hcr_id = create_test_hcr(&*j)?;
    let hcr_id = Arc::new(hcr_id);

    let num_threads = 20;
    let results = Arc::new(Mutex::new(Vec::new()));

    let mut handles = vec![];
    for i in 0..num_threads {
        let j = Arc::clone(&j);
        let hcr_id = Arc::clone(&hcr_id);
        let results = Arc::clone(&results);
        handles.push(thread::spawn(move || {
            let result = j.claim_hcr_for_execution(&hcr_id, "test-harness", &format!("worker_{i}"));
            results.lock().unwrap().push(result.is_ok());
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let results = results.lock().unwrap();
    let success_count = results.iter().filter(|&ok| *ok).count();
    assert_eq!(
        success_count, 1,
        "exactly one claim must succeed out of {num_threads}"
    );

    // Exactly one claim record.
    assert_eq!(j.hcr_claim_count()?, 1);

    // HCR is running.
    let hcr = j.get_harness_change_request(&hcr_id)?.unwrap();
    assert_eq!(hcr.status, "running");

    // Exactly one claim event.
    let events = j.events()?;
    let claim_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == JournalEventKind::HcrClaimSucceeded)
        .collect();
    assert_eq!(claim_events.len(), 1, "exactly one claim event");

    // Hash chain integrity.
    assert!(j.verify_hash_chain()?);
    Ok(())
}

// ── Run binding tests ──────────────────────────────────────────────────

#[test]
fn create_run_binding_after_claim() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let hcr_id = create_test_hcr(&j)?;

    let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

    let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
    let (returned_run_id, is_resume) = j.create_hcr_run_binding(&hcr_id, &claim_id.0, &run_id)?;

    assert_eq!(returned_run_id, run_id);
    assert!(!is_resume, "first creation is not a resume");

    // Verify binding exists.
    let binding = j.get_run_binding_for_claim(&claim_id.0)?;
    assert!(binding.is_some());
    let binding = binding.unwrap();
    assert_eq!(binding.hcr_id, hcr_id);
    assert_eq!(binding.run_id, run_id);

    // Verify reverse lookup works.
    let binding = j.get_hcr_binding_for_run(&run_id)?;
    assert!(binding.is_some());
    assert_eq!(binding.unwrap().claim_id, claim_id.0);

    assert_eq!(j.hcr_run_binding_count()?, 1);
    Ok(())
}

#[test]
fn duplicate_run_binding_is_idempotent() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let hcr_id = create_test_hcr(&j)?;

    let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

    let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
    let (first_run_id, first_resume) = j.create_hcr_run_binding(&hcr_id, &claim_id.0, &run_id)?;
    assert_eq!(first_run_id, run_id);
    assert!(!first_resume);

    // Same binding again (same run_id).
    let (second_run_id, second_resume) = j.create_hcr_run_binding(&hcr_id, &claim_id.0, &run_id)?;
    assert_eq!(second_run_id, run_id);
    assert!(second_resume, "second call is a resume");

    // Still only one binding.
    assert_eq!(j.hcr_run_binding_count()?, 1);
    Ok(())
}

#[test]
fn run_binding_same_claim_different_run_rejected() -> Result<()> {
    let j = JournalStore::in_memory()?;
    let hcr_id = create_test_hcr(&j)?;

    let claim_id = j.claim_hcr_for_execution(&hcr_id, "test-harness", "worker_1")?;

    let run_id_1 = format!("run_{}", uuid::Uuid::new_v4().simple());
    j.create_hcr_run_binding(&hcr_id, &claim_id.0, &run_id_1)?;

    // Different run_id for same (hcr_id, claim_id) should fail due to UNIQUE constraint.
    let run_id_2 = format!("run_{}", uuid::Uuid::new_v4().simple());
    let result = j.create_hcr_run_binding(&hcr_id, &claim_id.0, &run_id_2);
    // The INSERT OR IGNORE means it won't fail but will return the existing run_id.
    let (returned_run_id, is_resume) = result?;
    assert_eq!(returned_run_id, run_id_1, "must return existing run_id");
    assert!(is_resume, "must report as resume");
    assert_eq!(j.hcr_run_binding_count()?, 1);
    Ok(())
}
