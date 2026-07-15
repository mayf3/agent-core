//! PR3A North Star E2E using the real Harness and Linux bubblewrap gates.

#![cfg(target_os = "linux")]

use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
use agent_core_kernel::domain::capability_change::ProposalStatus;
use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::server::{coding_router, coding_task_submit};
use anyhow::Result;
use chrono::Utc;
use coding_harness::config::CodingConfig;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

#[path = "calculator_helpers.rs"]
mod helpers;

#[test]
fn authenticated_sentence_creates_real_pending_proposal() -> Result<()> {
    let artifact_root = unique_temp_dir("pr3a-real-artifacts");
    std::fs::create_dir_all(&artifact_root)?;

    let listener =
        TcpListener::bind("127.0.0.1:7200").expect("PR3A E2E requires exclusive 127.0.0.1:7200");
    let harness_config = CodingConfig {
        workspaces: HashMap::new(),
        kernel_api_url: "http://127.0.0.1:0".to_string(),
        capability_submit_token: "unused".to_string(),
        artifact_root: artifact_root.clone(),
        hcr_profiles: HashMap::new(),
        hcr_token: String::new(),
    };
    thread::spawn(move || coding_harness::server::serve(listener, Arc::new(harness_config)));
    thread::sleep(Duration::from_millis(100));

    let journal = JournalStore::in_memory()?;
    let config = helpers::kcfg(&artifact_root);
    let gateway = Gateway::new(config.clone());
    let session = journal.get_or_create_session(&SessionTarget {
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Feishu,
        conversation_key: "oc_pr3a_north_star".to_string(),
    })?;
    let snapshot_id = journal.current_registry_snapshot_id()?;
    let run = Run {
        id: RunId::new(),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:owner".to_string()),
            subject: PrincipalSubject::FeishuOpenId("owner".to_string()),
            source: PrincipalSource::Feishu,
            grants: vec![CapabilityGrant {
                operation: "external.coding_task_submit".to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("owner".to_string()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: snapshot_id.clone(),
        mode: RunMode::Default,
    };
    journal.insert_run(&run)?;

    let request = development_request(
        "开发一个 external.calculator，支持加减乘除",
        &run,
        &session,
        "om_pr3a_real_message",
    )?;
    let result = coding_task_submit::handle_coding_task_submit(
        &journal,
        &gateway,
        &config,
        &request,
        &run,
        &session,
        "om_pr3a_real_message",
    )?;

    assert_eq!(journal.harness_change_request_count()?, 1);
    let hcr = journal
        .get_harness_change_request(&result.hcr_id)?
        .expect("HCR must exist");
    assert_eq!(hcr.status, "succeeded");
    assert_eq!(hcr.session_id, session.id.0);
    assert_eq!(hcr.principal_id, run.principal.principal_id.0);

    let link = journal
        .load_proposal_hcr_link(&result.proposal_id)?
        .expect("trusted HCR link must exist");
    assert_eq!(link.operation, "external.calculator");
    assert_eq!(link.hcr_id, result.hcr_id);
    assert_eq!(link.candidate_id, result.candidate_id);
    assert_eq!(link.candidate_digest, result.candidate_digest);
    assert_eq!(link.artifact_ref, result.artifact_ref);
    assert_eq!(link.artifact_digest, result.artifact_digest);
    assert_eq!(link.evidence_digest, result.evidence_digest);
    assert_eq!(link.settlement_id, result.settlement_id);

    let proposal = journal
        .load_proposal(&result.proposal_id)?
        .expect("Proposal must exist");
    assert_eq!(proposal.status, ProposalStatus::PendingApproval);
    assert_eq!(proposal.origin_run_id, run.id);
    assert_eq!(proposal.origin_session_id, session.id);
    assert_eq!(proposal.expected_active_snapshot_id, snapshot_id);

    let store = ContentStore::new(artifact_root.clone());
    assert!(!store
        .load(&Sha256Digest::parse(&result.artifact_digest)?)?
        .is_empty());
    assert!(!store
        .load(&Sha256Digest::parse(&result.evidence_digest)?)?
        .is_empty());
    let manifest_digest = Sha256Digest::parse(&proposal.manifest_digest)?;
    let manifest: serde_json::Value = serde_json::from_slice(&store.load(&manifest_digest)?)?;
    assert_eq!(manifest["operation_name"], "external.calculator");
    assert_eq!(manifest["artifact_digest"], result.artifact_digest);

    let active = journal.load_registry_snapshot(&journal.current_registry_snapshot_id()?)?;
    assert!(active
        .operations
        .iter()
        .all(|operation| operation.name != "external.calculator"));
    assert!(journal.verify_hash_chain()?);

    same_message_twenty_way_is_exactly_once(&artifact_root, &config)?;

    std::fs::remove_dir_all(artifact_root).ok();
    Ok(())
}

fn same_message_twenty_way_is_exactly_once(
    artifact_root: &std::path::Path,
    base_config: &agent_core_kernel::config::KernelConfig,
) -> Result<()> {
    let journal = Arc::new(JournalStore::in_memory()?);
    let mut config = base_config.clone();
    config.harness_artifact_root = artifact_root.to_path_buf();
    let gateway = Arc::new(Gateway::new(config.clone()));
    let session = Arc::new(journal.get_or_create_session(&SessionTarget {
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Feishu,
        conversation_key: "oc_pr3a_concurrent".to_string(),
    })?);
    let run = Arc::new(Run {
        id: RunId::new(),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:owner".to_string()),
            subject: PrincipalSubject::FeishuOpenId("owner".to_string()),
            source: PrincipalSource::Feishu,
            grants: vec![CapabilityGrant {
                operation: "external.coding_task_submit".to_string(),
                scope: "current_session".to_string(),
            }],
            requester_id: Some("owner".to_string()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: journal.current_registry_snapshot_id()?,
        mode: RunMode::Default,
    });
    journal.insert_run(&run)?;
    let request = Arc::new(development_request(
        "开发一个 external.calculator，支持加减乘除",
        &run,
        &session,
        "om_pr3a_twenty_way",
    )?);
    let config = Arc::new(config);
    let barrier = Arc::new(Barrier::new(20));
    let mut workers = Vec::new();
    for _ in 0..20 {
        let journal = Arc::clone(&journal);
        let gateway = Arc::clone(&gateway);
        let config = Arc::clone(&config);
        let request = Arc::clone(&request);
        let run = Arc::clone(&run);
        let session = Arc::clone(&session);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            coding_task_submit::handle_coding_task_submit(
                &journal,
                &gateway,
                &config,
                request.as_ref(),
                &run,
                &session,
                "om_pr3a_twenty_way",
            )
        }));
    }
    let mut successes = Vec::new();
    for worker in workers {
        match worker.join().expect("concurrent submit thread") {
            Ok(result) => successes.push(result),
            Err(error) => assert!(
                error.to_string().contains("ALREADY_IN_PROGRESS"),
                "unexpected concurrent error: {error}"
            ),
        }
    }
    assert!(!successes.is_empty());
    let proposal_id = &successes[0].proposal_id;
    assert!(successes
        .iter()
        .all(|result| &result.proposal_id == proposal_id));
    assert_eq!(journal.harness_change_request_count()?, 1);
    let submit_receipts = journal
        .events()?
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::ReceiptReceived
                && event
                    .payload
                    .get("operation")
                    .and_then(serde_json::Value::as_str)
                    == Some("external.coding_task_submit")
        })
        .count();
    assert_eq!(
        submit_receipts, 1,
        "Harness submit invocation must be unique"
    );
    assert!(journal.load_proposal_hcr_link(proposal_id)?.is_some());
    assert!(journal.verify_hash_chain()?);
    Ok(())
}

#[test]
fn missing_submit_grant_fails_before_hcr_creation() -> Result<()> {
    let artifact_root = unique_temp_dir("pr3a-no-grant");
    let journal = JournalStore::in_memory()?;
    let config = helpers::kcfg(&artifact_root);
    let gateway = Gateway::new(config.clone());
    let session = journal.get_or_create_session(&SessionTarget {
        agent_id: config.agent_id.clone(),
        channel: ChannelKind::Feishu,
        conversation_key: "oc_pr3a_no_grant".to_string(),
    })?;
    let run = Run {
        id: RunId::new(),
        session_id: session.id.clone(),
        agent_id: session.agent_id.clone(),
        trigger_event_id: EventId::new(),
        principal: RunPrincipal {
            principal_id: PrincipalId("feishu:open_id:intruder".to_string()),
            subject: PrincipalSubject::FeishuOpenId("intruder".to_string()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: Some("intruder".to_string()),
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: journal.current_registry_snapshot_id()?,
        mode: RunMode::Default,
    };
    journal.insert_run(&run)?;
    let request = development_request(
        "开发计算器，实现四则运算",
        &run,
        &session,
        "om_pr3a_intruder",
    )?;
    let error = coding_task_submit::handle_coding_task_submit(
        &journal,
        &gateway,
        &config,
        &request,
        &run,
        &session,
        "om_pr3a_intruder",
    )
    .expect_err("missing grant must fail closed");
    assert!(error.to_string().contains("capability_not_enabled"));
    assert_eq!(journal.harness_change_request_count()?, 0);
    Ok(())
}

fn development_request(
    text: &str,
    run: &Run,
    session: &Session,
    source_message_id: &str,
) -> Result<DevelopmentRequest> {
    let intent = coding_router::parse_coding_intent(text)?;
    Ok(DevelopmentRequest::from_draft(
        intent.development_request,
        run.principal.principal_id.0.clone(),
        session.id.0.clone(),
        source_message_id.to_string(),
        format!("development:{source_message_id}"),
        CONTRACT_CATALOG_VERSION.to_string(),
    )?)
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}
