//! PR3B North Star: one Feishu sentence develops and activates a calculator,
//! then a fresh Feishu sentence executes it through the real Capability Host.

#![cfg(target_os = "linux")]

#[path = "calculator_helpers.rs"]
mod helpers;
#[path = "pr3b_north_star/model_stub.rs"]
mod model_stub;
#[path = "pr3b_north_star/support.rs"]
mod support;

use agent_core_kernel::domain::{JournalEvent, JournalEventKind};
use agent_core_kernel::journal::JournalStore;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use support::*;

#[test]
fn one_sentence_develops_activates_and_executes_calculator() -> Result<()> {
    if run_in_isolated_network_if_needed()? {
        return Ok(());
    }
    require_real_linux_sandbox()?;
    require_fixed_port(7200, "Coding Harness")?;
    require_fixed_port(7300, "Capability Host")?;

    let root = unique_temp_dir("pr3b-north-star");
    let artifact_root = root.join("cas");
    let db_path = root.join("journal.sqlite3");
    std::fs::create_dir_all(&artifact_root)?;

    let connector = MockFeishuSender::start()?;
    let model = model_stub::ModelStub::start(vec![
        model_stub::tool_call(
            "call_submit_calculator",
            "external.coding_task_submit",
            json!({
                "development_request": {
                    "target_kind": "invocable_capability",
                    "name": "external.calculator",
                    "requirements": ["provide add, subtract, multiply, and divide operations"],
                    "required_contracts": ["component.invoke.v0"],
                    "acceptance_criteria": ["multiply 6 by 7 returns 42"]
                }
            }),
        ),
        model_stub::text_reply("Proposal ready for approval."),
        model_stub::tool_call(
            "call_calculator_multiply",
            "external.calculator",
            json!({"operation":"multiply","a":6,"b":7}),
        ),
        model_stub::text_reply("42"),
    ])?;
    configure_host_clients(&artifact_root);
    start_harness(&artifact_root)?;
    start_capability_host()?;
    let kernel_port = free_port()?;
    start_kernel(
        kernel_port,
        &db_path,
        &artifact_root,
        connector.port,
        model.port,
    )?;

    let kernel = format!("http://127.0.0.1:{kernel_port}");
    wait_for_health(&kernel, Duration::from_secs(20))?;
    let journal = open_journal(&db_path)?;
    let s0 = journal.current_registry_snapshot_id()?;
    assert!(
        journal
            .load_registry_snapshot(&s0)?
            .lookup("external.calculator")
            .is_none(),
        "S0 must not contain external.calculator"
    );

    let first_ingress = feishu_ingress(
        "evt_pr3b_develop",
        "om_pr3b_develop",
        "开发一个 external.calculator，支持加减乘除",
    );
    let accepted = http_json(
        "POST",
        &format!("{kernel}/v1/ingress"),
        IPC_TOKEN,
        Some(&first_ingress),
    )?;
    assert_eq!(accepted.status, 200, "first ingress: {}", accepted.body);
    assert_eq!(accepted.body["status"], "accepted");

    let proposal_id = wait_for_value(Duration::from_secs(300), || {
        connector.messages().iter().find_map(pending_proposal_id)
    })
    .context("pending Proposal card was not sent to the Feishu connector")?;

    let proposal = http_json(
        "GET",
        &format!("{kernel}/v1/capability-change-proposals/{proposal_id}"),
        DECISION_TOKEN,
        None,
    )?;
    assert_eq!(proposal.status, 200, "proposal GET: {}", proposal.body);
    assert_eq!(proposal.body["status"], "PendingApproval");
    assert_eq!(proposal.body["operation_name"], "external.calculator");
    let approval = proposal
        .body
        .get("approval")
        .filter(|value| value.is_object())
        .context("trusted Approval missing from Proposal")?;
    assert_eq!(approval["principal_id"], OWNER_PRINCIPAL);
    assert_eq!(approval["expected_source_snapshot_id"], s0);
    assert_eq!(approval["status"], "Pending");

    let decision = json!({
        "decision": "approved",
        "approval_id": required_string(approval, "approval_id")?,
        "decision_nonce": required_string(approval, "decision_nonce")?,
        "principal_id": OWNER_PRINCIPAL,
        "expected_source_snapshot_id": required_string(approval, "expected_source_snapshot_id")?,
        "candidate_digest": required_string(approval, "candidate_digest")?,
        "artifact_digest": required_string(approval, "artifact_digest")?,
        "manifest_digest": required_string(approval, "manifest_digest")?,
    });
    let activated = http_json(
        "POST",
        &format!("{kernel}/v1/capability-change-proposals/{proposal_id}/decision"),
        DECISION_TOKEN,
        Some(&decision),
    )?;
    assert_eq!(activated.status, 200, "decision: {}", activated.body);
    assert_eq!(activated.body["status"], "Activated");
    assert_eq!(activated.body["replayed"], false);
    let deployment_id = required_string(&activated.body, "host_deployment_id")?;
    assert!(deployment_id.starts_with("chd_"));
    let s1 = required_string(&activated.body, "activated_snapshot_id")?;
    assert_ne!(s1, s0);
    let deployed = std::fs::read_dir(artifact_root.join(".capability-host"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .find(|record| record["operation_name"] == "external.calculator")
        .context("durable external.calculator deployment record missing")?;
    assert_eq!(deployed["deployment_id"], deployment_id);
    assert_eq!(deployed["proposal_id"], proposal_id);
    assert_eq!(deployed["target_registry_snapshot_id"], s1);
    assert!(required_string(&deployed, "probe_execution_id")?.starts_with("che_"));
    // JournalStore caches the active Registry identity per process instance;
    // reopen exactly as a restarted Kernel would to observe the committed CAS.
    let journal = open_journal(&db_path)?;
    assert_eq!(journal.current_registry_snapshot_id()?, s1);
    assert!(
        journal
            .load_registry_snapshot(&s1)?
            .lookup("external.calculator")
            .is_some(),
        "S1 must contain external.calculator"
    );
    let grants = journal.load_active_external_operation_grants(
        OWNER_PRINCIPAL,
        "Feishu",
        "p2p",
        "principal_channel",
        &s1,
    )?;
    assert_eq!(grants.len(), 1, "owner must receive exactly one S1 grant");
    assert_eq!(grants[0].operation, "external.calculator");

    let second_ingress = feishu_ingress(
        "evt_pr3b_calculate",
        "om_pr3b_calculate",
        "用 external.calculator 计算 6 * 7",
    );
    let accepted = http_json(
        "POST",
        &format!("{kernel}/v1/ingress"),
        IPC_TOKEN,
        Some(&second_ingress),
    )?;
    assert_eq!(accepted.status, 200, "second ingress: {}", accepted.body);
    assert_eq!(accepted.body["status"], "accepted");

    wait_for_value(Duration::from_secs(60), || {
        connector.messages().iter().find_map(|message| {
            (message.pointer("/arguments/text").and_then(Value::as_str) == Some("42")).then_some(())
        })
    })
    .context("production Connector delivery did not send final text 42")?;

    assert_north_star_journal(&journal, &s1)?;
    assert!(
        journal.verify_hash_chain()?,
        "Journal hash chain must verify"
    );
    model.assert_exhausted()?;
    std::fs::remove_dir_all(root).ok();
    Ok(())
}

fn open_journal(path: &std::path::Path) -> Result<JournalStore> {
    let journal = JournalStore::open(path)?;
    journal.initialize_registry()?;
    Ok(journal)
}

fn assert_north_star_journal(journal: &JournalStore, s1: &str) -> Result<()> {
    let events = journal.events()?;
    let gates: std::collections::BTreeSet<_> = events
        .iter()
        .filter(|event| {
            event.kind == JournalEventKind::InvocationProposed
                && event.payload.get("operation").and_then(Value::as_str)
                    == Some("external.coding_hcr_accept")
        })
        .filter_map(|event| event.payload.get("gate_kind").and_then(Value::as_str))
        .collect();
    assert_eq!(
        gates,
        [
            "artifact",
            "build",
            "scaffold",
            "trusted_smoke",
            "trusted_test"
        ]
        .into_iter()
        .collect(),
        "all five real Harness gates must be journaled"
    );
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::CapabilityChangeApproved));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::CapabilityChangeActivated));
    assert!(events.iter().any(|event| {
        event.kind == JournalEventKind::ExternalOperationGranted
            && event.payload.get("operation").and_then(Value::as_str) == Some("external.calculator")
            && event
                .payload
                .get("grantee_principal_id")
                .and_then(Value::as_str)
                == Some(OWNER_PRINCIPAL)
    }));
    let final_reply = events
        .iter()
        .rev()
        .find(|event| {
            event.kind == JournalEventKind::AssistantReplyDelivered
                && event.payload.get("text").and_then(Value::as_str) == Some("42")
        })
        .context("final AssistantReplyDelivered(42) missing")?;
    let run_id = final_reply
        .run_id
        .as_ref()
        .context("final reply has no Run")?;
    let run = journal
        .get_run(&run_id.0)?
        .context("fresh calculator Run missing")?;
    assert_eq!(run.registry_snapshot_id, s1, "new Run must pin S1");
    assert_eq!(run.principal.principal_id.0, OWNER_PRINCIPAL);
    assert!(run
        .principal
        .grants
        .iter()
        .any(|grant| grant.operation == "external.calculator"));

    let proposed = find_run_operation_event(
        &events,
        &run_id.0,
        JournalEventKind::InvocationProposed,
        "external.calculator",
    )
    .context("calculator InvocationProposed missing")?;
    let invocation_id = proposed
        .correlation_id
        .as_deref()
        .context("calculator invocation correlation missing")?;
    assert!(events.iter().any(|event| {
        event.run_id.as_ref().map(|id| id.0.as_str()) == Some(run_id.0.as_str())
            && event.kind == JournalEventKind::InvocationApproved
            && event.correlation_id.as_deref() == Some(invocation_id)
            && event.payload.get("operation").and_then(Value::as_str) == Some("external.calculator")
    }));
    let receipt = events
        .iter()
        .find(|event| {
            event.run_id.as_ref().map(|id| id.0.as_str()) == Some(run_id.0.as_str())
                && event.kind == JournalEventKind::ReceiptReceived
                && event.correlation_id.as_deref() == Some(invocation_id)
        })
        .context("calculator ReceiptReceived missing")?;
    assert_eq!(receipt.payload["status"], "Succeeded");
    assert_eq!(receipt.payload["output"], 42);
    assert!(required_string(&receipt.payload, "external_ref")?.starts_with("che_"));
    Ok(())
}

fn find_run_operation_event<'a>(
    events: &'a [JournalEvent],
    run_id: &str,
    kind: JournalEventKind,
    operation: &str,
) -> Option<&'a JournalEvent> {
    events.iter().find(|event| {
        event.run_id.as_ref().map(|id| id.0.as_str()) == Some(run_id)
            && event.kind == kind
            && event.payload.get("operation").and_then(Value::as_str) == Some(operation)
    })
}
