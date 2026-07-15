use super::*;
use crate::domain::*;
use rusqlite::params;

#[test]
fn managed_service_versions_are_monotonic() {
    assert!(compare_version("0.2.0", "0.1.9").is_gt());
    assert!(compare_version("1.0.0", "0.99.99").is_gt());
    assert!(compare_version("0.1.0", "0.1.0").is_eq());
}

#[test]
fn healthy_receipt_atomically_activates_component_snapshot() -> Result<()> {
    const CANDIDATE: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ARTIFACT: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MANIFEST: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const EVIDENCE: &str =
        "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const PAYLOAD: &str = "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let journal = super::super::JournalStore::in_memory()?;
    let agent = AgentId("main".into());
    let principal_id = "feishu:open_id:owner";
    let session = journal.get_or_create_session(&SessionTarget {
        agent_id: agent.clone(),
        channel: ChannelKind::Feishu,
        conversation_key: principal_id.into(),
    })?;
    let source_registry = journal.current_registry_snapshot_id()?;
    let run = Run {
        id: RunId("run_service_activation".into()),
        session_id: session.id.clone(),
        agent_id: agent.clone(),
        trigger_event_id: EventId("event_service_activation".into()),
        principal: RunPrincipal {
            principal_id: PrincipalId(principal_id.into()),
            subject: PrincipalSubject::FeishuOpenId("owner".into()),
            source: PrincipalSource::Feishu,
            grants: vec![],
            requester_id: None,
        },
        parent_run_id: None,
        delegated_by: None,
        status: RunStatus::Completed,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        registry_snapshot_id: source_registry.clone(),
        mode: RunMode::Default,
    };
    journal.insert_run(&run)?;
    let (hcr_id, _) = journal.create_harness_change_request(
        "CodingRouter",
        "message_service_activation",
        &session.id.0,
        principal_id,
        "Feishu",
        "p2p",
        "coding-harness-v0",
        "managed service",
    )?;
    let claim = journal.claim_hcr_for_execution(&hcr_id, "coding-harness-v0", "worker")?;
    for (index, gate) in [
        "scaffold",
        "build",
        "trusted_test",
        "trusted_smoke",
        "artifact",
    ]
    .iter()
    .enumerate()
    {
        let attempt = format!("attempt_{index}");
        journal.insert_gate_attempt(
            &attempt,
            &hcr_id,
            &claim.0,
            &run.id.0,
            "coding-harness-v0",
            "generated",
            gate,
            "external.coding_hcr_accept",
            "coding-hcr-v1",
            &format!("intent_{index}"),
            &Utc::now().to_rfc3339(),
        )?;
        journal.insert_evidence_atomically(
            &format!("evidence_{index}"),
            &attempt,
            &format!("receipt_event_{index}"),
            PAYLOAD,
            &Utc::now().to_rfc3339(),
        )?;
    }
    let settlement_id = "settlement_service_activation";
    {
        let conn = journal.conn.lock().map_err(|_| anyhow::anyhow!("mutex"))?;
        conn.execute(
            "UPDATE harness_change_requests SET status='succeeded',run_id=?1 WHERE request_id=?2",
            params![run.id.0, hcr_id],
        )?;
        conn.execute(
            "INSERT INTO hcr_settlements
             (settlement_id,hcr_id,claim_id,run_id,result,error_code,evidence_set_digest,created_at)
             VALUES (?1,?2,?3,?4,'succeeded',NULL,?5,?6)",
            params![
                settlement_id,
                hcr_id,
                claim.0,
                run.id.0,
                EVIDENCE,
                Utc::now().to_rfc3339()
            ],
        )?;
        conn.execute(
            "INSERT INTO hcr_receipt_identities
             (hcr_id,claim_id,run_id,idempotency_key,payload_digest,receipt_event_id,
              harness_execution_id,overall_outcome,candidate_digest,artifact_ref,
              artifact_digest,evidence_digest,candidate_id,invocation_id)
             VALUES (?1,?2,?3,'accept',?4,'receipt','execution','CandidatePassed',
                     ?5,?6,?6,?7,'candidate','acceptance_invocation')",
            params![hcr_id, claim.0, run.id.0, PAYLOAD, CANDIDATE, ARTIFACT, EVIDENCE],
        )?;
    }

    let mut service = ServiceManifest {
        schema_version: crate::domain::SERVICE_MANIFEST_SCHEMA.into(),
        manifest_id: String::new(),
        component_id: "token-dashboard".into(),
        kind: TargetKind::HookConsumerService,
        artifact_digest: ARTIFACT.into(),
        entrypoint: "artifact".into(),
        runtime_profile: "managed-service-v0".into(),
        version: "0.1.0".into(),
        required_contracts: vec!["event.observe.v0".into()],
        requested_permissions: vec!["journal.observe".into()],
        listen_policy: ListenPolicy {
            host: "127.0.0.1".into(),
            port: 0,
            exposure: "loopback".into(),
        },
        healthcheck: ServiceHealthcheck {
            method: "GET".into(),
            path: "/health".into(),
            timeout_ms: 5_000,
        },
        state_path: "state".into(),
        upgrade_policy: UpgradePolicy {
            strategy: "replace_after_ready".into(),
            require_healthy_before_switch: true,
        },
        rollback_policy: RollbackPolicy {
            retain_previous_versions: 2,
            automatic_on_health_failure: true,
        },
    };
    service.manifest_id = service.compute_manifest_id()?;
    let proposal_id = "proposal_service_activation";
    let proposal = crate::domain::capability_change::CapabilityChangeProposal::new(
        proposal_id.into(),
        principal_id.into(),
        agent.clone(),
        session.id.clone(),
        run.id.clone(),
        ARTIFACT.into(),
        ARTIFACT.into(),
        service.manifest_id.clone(),
        MANIFEST.into(),
        EVIDENCE.into(),
        EVIDENCE.into(),
        vec![service.component_id.clone()],
        "managed service gates passed".into(),
        source_registry.clone(),
    );
    let link = CapabilityProposalHcrLink {
        proposal_id: proposal_id.into(),
        hcr_id: hcr_id.clone(),
        claim_id: claim.0.clone(),
        run_id: run.id.0.clone(),
        operation: service.component_id.clone(),
        candidate_id: "candidate".into(),
        candidate_digest: CANDIDATE.into(),
        artifact_ref: ARTIFACT.into(),
        artifact_digest: ARTIFACT.into(),
        evidence_digest: EVIDENCE.into(),
        source_registry_snapshot_id: source_registry.clone(),
        settlement_id: settlement_id.into(),
        created_at: Utc::now().to_rfc3339(),
    };
    journal.create_proposal_with_hcr_link(&proposal, &link)?;
    let approval = journal
        .load_capability_approval_by_proposal(proposal_id)?
        .expect("approval");
    let identity = TrustedDecisionIdentity {
        proposal_id: proposal_id.into(),
        approval_id: approval.approval_id,
        decision_nonce: approval.decision_nonce,
        principal_id: principal_id.into(),
        expected_source_snapshot_id: source_registry,
        candidate_digest: CANDIDATE.into(),
        artifact_digest: ARTIFACT.into(),
        manifest_digest: MANIFEST.into(),
        decision_id: "decision_service_activation".into(),
        payload_digest: PAYLOAD.into(),
    };
    let mut intent = DeploymentIntent {
        protocol_version: crate::domain::DEPLOYMENT_PROTOCOL.into(),
        invocation_id: "deployment_invocation_service_activation".into(),
        intent_id: String::new(),
        proposal_id: proposal_id.into(),
        decision_id: identity.decision_id.clone(),
        service_manifest_digest: MANIFEST.into(),
        artifact_digest: ARTIFACT.into(),
        expected_version: service.version.clone(),
        action: "install_start".into(),
    };
    intent.intent_id = intent.expected_intent_id();
    journal.record_trusted_service_deployment_intent(&identity, &intent, &service, &agent)?;
    let mut receipt = DeploymentReceipt {
        protocol_version: crate::domain::DEPLOYMENT_PROTOCOL.into(),
        receipt_id: String::new(),
        invocation_id: intent.invocation_id.clone(),
        intent_id: intent.intent_id.clone(),
        proposal_id: proposal_id.into(),
        decision_id: identity.decision_id.clone(),
        deployment_id: intent.deployment_id(&service.component_id),
        component_id: service.component_id.clone(),
        service_manifest_digest: MANIFEST.into(),
        artifact_digest: ARTIFACT.into(),
        version: service.version.clone(),
        status: "healthy".into(),
        endpoint: "http://127.0.0.1:7401".into(),
        health_status: "ready".into(),
        log_ref: "components/token-dashboard/logs/0.1.0.log".into(),
        previous_artifact_digest: None,
        started_at: Utc::now().to_rfc3339(),
        finished_at: Utc::now().to_rfc3339(),
        replayed: false,
    };
    receipt.receipt_id = receipt.expected_receipt_id();
    let result =
        journal.activate_trusted_service_atomic(&identity, &intent, &service, &receipt, &agent)?;
    assert_eq!(result.status, CapabilityApprovalStatus::Approved);
    let active = journal.load_component_registry_snapshot(
        result.activated_snapshot_id.as_deref().expect("snapshot"),
    )?;
    let component = active.lookup("token-dashboard").expect("component");
    assert_eq!(component.endpoint, receipt.endpoint);
    assert_eq!(component.deployment_receipt_id, receipt.receipt_id);
    assert_eq!(
        journal
            .load_proposal(proposal_id)?
            .expect("proposal")
            .status,
        crate::domain::capability_change::ProposalStatus::Activated
    );
    let events = journal.events()?;
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::DeploymentIntentRecorded));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::DeploymentReceiptRecorded));
    assert!(events
        .iter()
        .any(|event| event.kind == JournalEventKind::ComponentRegistered));
    Ok(())
}
