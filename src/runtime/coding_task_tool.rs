use super::tool_loop::ToolCallOutcome;
use crate::contract_catalog::{ContractCatalog, CONTRACT_CATALOG_VERSION};
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use crate::llm::LlmClient;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelDevelopmentRequest {
    target_kind: TargetKind,
    name: String,
    requirements: Vec<String>,
    required_contracts: Vec<String>,
    acceptance_criteria: Vec<String>,
}

/// Stable detail codes that signal a contract discoverability gap. When the
/// dispatch layer sees either of these in a failed submit, it attaches the
/// `contract_discovery` object so the caller can resubmit without a human.
const DETAIL_UNKNOWN_CONTRACT: &str = "DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN";
const DETAIL_INVALID_REQUIRED_CONTRACTS: &str = "DEVELOPMENT_REQUEST_INVALID_REQUIRED_CONTRACTS";

/// Structured error carrying the contracts the caller needs to recover from a
/// `DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN` submission. Produced at seal time and
/// downcast in the dispatch layer; it intentionally never reaches the wire as a
/// free-form string (the catalog ids contain `.` and lowercase letters that the
/// legacy `safe_detail_code` renderer would discard).
#[derive(Debug)]
struct ContractDiscoveryError {
    detail_code: &'static str,
    unknown_contracts: Vec<String>,
    known_contracts: Vec<String>,
}

impl std::fmt::Display for ContractDiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The display text keeps the stable detail code first so non-discovery
        // consumers (e.g. logs) still see a recognizable failure code.
        write!(f, "{}", self.detail_code)
    }
}

impl std::error::Error for ContractDiscoveryError {}

impl<L: LlmClient + 'static> super::Runtime<L> {
    pub(crate) fn dispatch_coding_task_submit(
        &self,
        approved: &ApprovedInvocation,
        journal: &JournalStore,
        gateway: &Gateway,
        run: &Run,
        session: &Session,
        correlation_id: &str,
    ) -> ToolCallOutcome {
        let result = approved
            .intent()
            .arguments
            .get("development_request")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("DEVELOPMENT_REQUEST_MISSING"))
            .and_then(|value| {
                serde_json::from_value::<ModelDevelopmentRequest>(value)
                    .map_err(|_| anyhow::anyhow!("INVALID_DEVELOPMENT_REQUEST"))
            })
            .and_then(|draft| seal_development_request(journal, run, session, draft))
            .and_then(|request| {
                crate::server::coding_task_submit::handle_coding_task_submit(
                    journal,
                    gateway,
                    &self.config,
                    &request,
                    run,
                    session,
                    &request.source_message_id,
                )
            })
            .and_then(|result| serde_json::to_value(result).map_err(Into::into));
        let (status, output, text) = match result {
            Ok(output) => (
                ReceiptStatus::Succeeded,
                output.clone(),
                serde_json::to_string(&json!({"status":"succeeded","result":output}))
                    .unwrap_or_else(|_| r#"{"status":"succeeded"}"#.into()),
            ),
            Err(error) => {
                let detail = safe_detail_code(&error.to_string());
                let discovery = contract_discovery(&error, &detail);
                // `text` mirrors `output` plus a `status` field, so build the
                // shared payload once and reuse it for both shapes.
                let output = match &discovery {
                    Some(discovery) => json!({
                        "error_category": "external_execution_failed",
                        "detail_code": detail,
                        "contract_discovery": discovery,
                    }),
                    None => json!({
                        "error_category": "external_execution_failed",
                        "detail_code": detail,
                    }),
                };
                let mut text_value = json!({
                    "status": "execution_failed",
                    "error_category": "external_execution_failed",
                    "detail_code": detail,
                });
                if let Some(discovery) = &discovery {
                    text_value["contract_discovery"] = discovery.clone();
                }
                let text = serde_json::to_string(&text_value)
                    .unwrap_or_else(|_| r#"{"status":"execution_failed"}"#.into());
                (ReceiptStatus::Failed, output, text)
            }
        };
        if journal
            .append_event(
                JournalEventKind::ReceiptReceived,
                Some(&run.id),
                Some(&session.id),
                Some(correlation_id),
                json!({
                    "invocation_id": approved.intent().invocation_id,
                    "operation": approved.intent().operation,
                    "status": format!("{:?}", status),
                    "output": output,
                }),
            )
            .is_err()
        {
            return ToolCallOutcome::Fatal {
                category: "journal_unwritable",
            };
        }
        ToolCallOutcome::ToolResult { text }
    }
}

fn seal_development_request(
    journal: &JournalStore,
    run: &Run,
    session: &Session,
    request: ModelDevelopmentRequest,
) -> anyhow::Result<DevelopmentRequest> {
    let ingress = journal
        .ingress_event_by_event_id(&run.trigger_event_id.0)?
        .ok_or_else(|| anyhow::anyhow!("DEVELOPMENT_REQUEST_SOURCE_EVENT_MISSING"))?;
    let source_message_id = ingress
        .payload
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("DEVELOPMENT_REQUEST_SOURCE_MESSAGE_MISSING"))?;
    let catalog = ContractCatalog::v1();
    let unknown: Vec<String> = catalog
        .unknown_contracts(&request.required_contracts)
        .into_iter()
        .map(str::to_owned)
        .collect();
    if !unknown.is_empty() {
        // Collect every unknown contract once so the caller can see the full
        // gap, then surface the catalog as the recovery hint. The detail code
        // stays stable; lowercase contract names are carried structurally.
        return Err(ContractDiscoveryError {
            detail_code: DETAIL_UNKNOWN_CONTRACT,
            unknown_contracts: unknown,
            known_contracts: catalog
                .known_contract_ids()
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
        .into());
    }
    let mut requested_permissions = Vec::new();
    for contract_id in &request.required_contracts {
        let contract = catalog.get(contract_id).expect(
            "unknown contracts were rejected above; remaining ids are guaranteed to resolve",
        );
        for permission in &contract.permissions {
            if !requested_permissions.contains(permission) {
                requested_permissions.push(permission.clone());
            }
        }
    }
    let mut draft = DevelopmentRequestDraft::new(request.target_kind, request.name);
    draft.requirements = request.requirements;
    draft.required_contracts = request.required_contracts;
    draft.requested_permissions = requested_permissions;
    draft.acceptance_criteria = request.acceptance_criteria;
    DevelopmentRequest::from_draft(
        draft,
        run.principal.principal_id.0.clone(),
        session.id.0.clone(),
        source_message_id.into(),
        format!("development:{source_message_id}"),
        CONTRACT_CATALOG_VERSION.into(),
    )
}

fn safe_detail_code(message: &str) -> String {
    message
        .split(|character: char| {
            !(character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_')
        })
        .filter(|value| value.len() >= 3 && value.bytes().any(|byte| byte.is_ascii_uppercase()))
        .next_back()
        .unwrap_or("CODING_TASK_SUBMIT_FAILED")
        .chars()
        .take(128)
        .collect()
}

/// Build the `contract_discovery` payload to attach to a failed submit.
///
/// Two recoverable failures carry it, keyed by the stable `detail_code`:
///  - `DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN` (downcast from the seal-time
///    `ContractDiscoveryError`, which knows the specific unknown ids), and
///  - `DEVELOPMENT_REQUEST_INVALID_REQUIRED_CONTRACTS` (empty array), which is
///    raised by domain validation and therefore matched here purely on its
///    detail code rather than by redefining the domain error system.
///
/// Both paths reuse the live `ContractCatalog::v1()` registry so the list is
/// always consistent with what the validator actually accepts.
fn contract_discovery(error: &anyhow::Error, detail_code: &str) -> Option<Value> {
    if let Some(discovery) = error.downcast_ref::<ContractDiscoveryError>() {
        return Some(json!({
            "unknown_contracts": discovery.unknown_contracts,
            "known_contracts": discovery.known_contracts,
        }));
    }
    if detail_code == DETAIL_INVALID_REQUIRED_CONTRACTS {
        let known: Vec<String> = ContractCatalog::v1()
            .known_contract_ids()
            .into_iter()
            .map(str::to_owned)
            .collect();
        // Empty array: there are no specific unknown ids, but the caller still
        // needs the catalog to choose a valid contract before resubmitting.
        let empty: Vec<String> = Vec::new();
        return Some(json!({
            "unknown_contracts": empty,
            "known_contracts": known,
        }));
    }
    None
}

#[cfg(test)]
mod tests {
    //! Contract discoverability for `external.coding_task_submit`.
    //!
    //! These tests exercise the seal-time error path end-to-end (a real
    //! `JournalStore`, `Run`, and `Session`), proving that:
    //!   - an unknown contract is still rejected, but the rejection now carries
    //!     the catalog so the caller can recover without a human,
    //!   - every unknown contract is reported (not just the first),
    //!   - an empty `required_contracts` is still rejected and carries the
    //!     catalog,
    //!   - a valid request still seals successfully (no regression), and
    //!   - an agent that reads `known_contracts` and resubmits succeeds.
    use super::*;
    use crate::contract_catalog::ContractCatalog;
    use crate::domain::{
        AgentId, ChannelKind, EventId, PrincipalId, PrincipalSource, PrincipalSubject, Run,
        RunMode, RunPrincipal, RunStatus, RunId, Session, SessionId, SessionStatus,
    };
    use crate::journal::JournalStore;
    use chrono::Utc;

    /// Build a journal seeded with one `IngressAccepted` event carrying a
    /// `message_id`, plus the `Run`/`Session` whose `trigger_event_id` points
    /// at it — the minimum `seal_development_request` needs.
    fn seal_fixture(
        message_id: &str,
    ) -> anyhow::Result<(JournalStore, Run, Session)> {
        let journal = JournalStore::in_memory()?;
        journal.initialize_registry()?;
        let trigger_event_id = EventId::new();
        journal.append_event(
            crate::domain::JournalEventKind::IngressAccepted,
            None,
            None,
            Some(&trigger_event_id.0),
            json!({
                "event_id": trigger_event_id.0,
                "message_id": message_id,
            }),
        )?;
        let now = Utc::now();
        let session = Session {
            id: SessionId::new(),
            agent_id: AgentId("test".into()),
            channel: ChannelKind::Cli,
            conversation_key: "test".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: now,
            status: SessionStatus::Active,
            version: 1,
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id.clone(),
            agent_id: session.agent_id.clone(),
            trigger_event_id,
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:test".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: None,
            },
            parent_run_id: None,
            delegated_by: None,
            status: RunStatus::Running,
            created_at: now,
            updated_at: now,
            registry_snapshot_id: "snap_test".into(),
            mode: RunMode::Default,
            budget_hook_id: None,
            budget_hook_version: None,
            budget_decision_digest: None,
            budget_max_tool_rounds: None,
            budget_max_wall_time_ms: None,
            budget_exhaustion_action: None,
        };
        Ok((journal, run, session))
    }

    fn draft(
        target: crate::domain::TargetKind,
        required_contracts: Vec<&str>,
    ) -> ModelDevelopmentRequest {
        ModelDevelopmentRequest {
            target_kind: target,
            name: "external.example".into(),
            requirements: vec!["provide a bounded invocation".into()],
            required_contracts: required_contracts.into_iter().map(String::from).collect(),
            acceptance_criteria: vec!["trusted contract tests pass".into()],
        }
    }

    /// Unknown contracts are rejected with a stable detail code AND a discovery
    /// payload that names every unknown id plus the full catalog.
    #[test]
    fn unknown_contract_is_rejected_with_known_contracts() -> anyhow::Result<()> {
        let (journal, run, session) = seal_fixture("msg_unknown")?;
        let err = seal_development_request(
            &journal,
            &run,
            &session,
            draft(crate::domain::TargetKind::InvocableCapability, vec![
                "component.invoke.v0", // valid
                "totally.made.up.v0",  // unknown
            ]),
        )
        .expect_err("unknown contract must be rejected");

        let discovery = err
            .downcast_ref::<ContractDiscoveryError>()
            .expect("error must be a ContractDiscoveryError");
        assert_eq!(discovery.detail_code, "DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN");
        assert_eq!(discovery.unknown_contracts, vec!["totally.made.up.v0"]);
        assert_eq!(
            discovery.known_contracts,
            ContractCatalog::v1()
                .known_contract_ids()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    /// Every unknown contract is reported, in request order, not just the first.
    #[test]
    fn multiple_unknown_contracts_all_reported_in_order() -> anyhow::Result<()> {
        let (journal, run, session) = seal_fixture("msg_multi_unknown")?;
        let err = seal_development_request(
            &journal,
            &run,
            &session,
            draft(crate::domain::TargetKind::InvocableCapability, vec![
                "missing.alpha.v0",
                "component.invoke.v0", // valid -> not reported
                "missing.beta.v0",
            ]),
        )
        .expect_err("must reject when any contract is unknown");
        let discovery = err.downcast_ref::<ContractDiscoveryError>().unwrap();
        assert_eq!(
            discovery.unknown_contracts,
            vec!["missing.alpha.v0", "missing.beta.v0"]
        );
        Ok(())
    }

    /// Empty `required_contracts` is still rejected. This error originates in
    /// domain validation (`ensure_nonempty_unique`), so it surfaces as a plain
    /// `DEVELOPMENT_REQUEST_INVALID_REQUIRED_CONTRACTS` rather than a
    /// `ContractDiscoveryError`; the dispatch render layer attaches the catalog
    /// via the stable detail code (see `contract_discovery`).
    #[test]
    fn empty_required_contracts_is_rejected() -> anyhow::Result<()> {
        let (journal, run, session) = seal_fixture("msg_empty")?;
        let err = seal_development_request(
            &journal,
            &run,
            &session,
            draft(crate::domain::TargetKind::InvocableCapability, vec![]),
        )
        .expect_err("empty required_contracts must be rejected");
        // Domain validation fires after the unknown-contract pre-check (which
        // is a no-op for an empty list), so this is the plain domain error.
        assert_eq!(
            safe_detail_code(&err.to_string()),
            "DEVELOPMENT_REQUEST_INVALID_REQUIRED_CONTRACTS"
        );
        // The render layer keys off this detail code to attach the catalog.
        let discovery = contract_discovery(&err, "DEVELOPMENT_REQUEST_INVALID_REQUIRED_CONTRACTS");
        let discovery = discovery.expect("empty-array path must attach discovery");
        assert_eq!(discovery["unknown_contracts"].as_array().unwrap().len(), 0);
        assert_eq!(
            discovery["known_contracts"].as_array().unwrap().len(),
            ContractCatalog::v1().known_contract_ids().len()
        );
        Ok(())
    }

    /// A request whose contracts are all known seals successfully — no
    /// regression in the happy path.
    #[test]
    fn valid_request_seals_successfully() -> anyhow::Result<()> {
        let (journal, run, session) = seal_fixture("msg_valid")?;
        let request = seal_development_request(
            &journal,
            &run,
            &session,
            draft(crate::domain::TargetKind::InvocableCapability, vec![
                "component.invoke.v0",
            ]),
        )?;
        assert_eq!(
            request.contract_catalog_version,
            crate::contract_catalog::CONTRACT_CATALOG_VERSION
        );
        assert_eq!(request.required_contracts, vec!["component.invoke.v0"]);
        // Permissions are derived from the contract descriptor.
        assert_eq!(request.requested_permissions, vec!["component.invoke"]);
        Ok(())
    }

    /// An agent that first submits an unknown contract, reads the returned
    /// `known_contracts`, and resubmits with a valid one succeeds. This is the
    /// acceptance proof for AGENT_CAN_DISCOVER_CONTRACTS_WITHOUT_HUMAN.
    #[test]
    fn agent_reads_known_contracts_then_resubmits_successfully() -> anyhow::Result<()> {
        let (journal, run, session) = seal_fixture("msg_resubmit")?;

        // Attempt 1: unknown contract -> rejected, but discoverable.
        let first = seal_development_request(
            &journal,
            &run,
            &session,
            draft(crate::domain::TargetKind::InvocableCapability, vec![
                "route.harness.v0", // plausible-sounding but unknown
            ]),
        );
        let first_err = first.expect_err("first attempt must be rejected");
        let discovery = first_err
            .downcast_ref::<ContractDiscoveryError>()
            .expect("rejected attempt must carry discovery");
        assert_eq!(discovery.detail_code, "DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN");

        // The agent reads the catalog and picks a real contract for an
        // invocable capability.
        let learned: Vec<String> = discovery.known_contracts.clone();
        assert!(learned.contains(&"component.invoke.v0".to_string()));

        // Attempt 2: resubmit using the discovered contract.
        let recovered = seal_development_request(
            &journal,
            &run,
            &session,
            draft(crate::domain::TargetKind::InvocableCapability, vec![
                "component.invoke.v0",
            ]),
        )
        .expect("resubmission with a discovered contract must succeed");
        assert_eq!(recovered.required_contracts, vec!["component.invoke.v0"]);
        Ok(())
    }

    /// `contract_discovery` attaches the payload only for the two recoverable
    /// contract failures and returns `None` for unrelated errors (so unrelated
    /// failures are not polluted with a catalog).
    #[test]
    fn contract_discovery_only_attached_to_contract_failures() {
        let unrelated = anyhow::anyhow!("DEVELOPMENT_REQUEST_SOURCE_EVENT_MISSING");
        assert!(contract_discovery(&unrelated, "DEVELOPMENT_REQUEST_SOURCE_EVENT_MISSING")
            .is_none());

        let unknown = ContractDiscoveryError {
            detail_code: "DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN",
            unknown_contracts: vec!["x".into()],
            known_contracts: vec!["y".into()],
        }
        .into();
        let discovery = contract_discovery(&unknown, "DEVELOPMENT_REQUEST_CONTRACT_UNKNOWN")
            .expect("unknown-contract path attaches discovery");
        assert_eq!(discovery["unknown_contracts"], json!(["x"]));
        assert_eq!(discovery["known_contracts"], json!(["y"]));
    }
}
