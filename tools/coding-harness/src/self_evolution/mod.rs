pub mod acceptance_kit;
pub mod acceptance_selector;
pub mod artifact_manifest;
mod component_profile;
mod generator;
mod profile_acceptance;
mod submission_store;

pub(crate) use generator::invocable::contract::CapabilityContract as InvocableCapabilityContract;
pub(crate) use generator::invocable::process_input as invocable_process_input;

use agent_core_kernel::contract_catalog::ContractCatalog;
use agent_core_kernel::domain::{ComponentLifecycleState, DevelopmentRequest, TargetKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

pub use component_profile::{ComponentProfile, ComponentProfileCatalog};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevelopmentPlan {
    pub request_id: String,
    pub contract_catalog_version: String,
    pub component_profile_id: String,
    pub target_kind: TargetKind,
    pub lifecycle_state: ComponentLifecycleState,
}

pub fn handle_submit(artifact_root: &Path, args: &Value) -> Value {
    let invocation_id = args
        .get("invocation_intent_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected_attempt_key = format!("development-attempt:{invocation_id}");
    let attempt_key = match args.get("idempotency_key").and_then(Value::as_str) {
        Some(value) if invocation_id.starts_with("attempt_") && value == expected_attempt_key => {
            value
        }
        None => return error("MISSING_OR_INVALID_ATTEMPT_KEY"),
        _ => return error("MISSING_OR_INVALID_ATTEMPT_KEY"),
    };
    let store = submission_store::SubmissionStore::new(artifact_root);
    // This single persisted closure contains plan, candidate generation, and
    // every build/test/acceptance gate. Replays never enter it a second time.
    match store.execute(attempt_key, args, || {
        handle_submit_once(artifact_root, args, attempt_key)
    }) {
        submission_store::SubmissionExecution::Completed(result) => result,
        submission_store::SubmissionExecution::InProgress => submission_store::in_progress(),
        submission_store::SubmissionExecution::OutcomeUnknown => {
            submission_store::outcome_unknown()
        }
    }
}

fn handle_submit_once(artifact_root: &Path, args: &Value, attempt_key: &str) -> Value {
    let request_value = match args.get("development_request") {
        Some(value) => value,
        None => return error("MISSING_DEVELOPMENT_REQUEST"),
    };
    let request: DevelopmentRequest = match serde_json::from_value(request_value.clone()) {
        Ok(request) => request,
        Err(_) => return error("INVALID_DEVELOPMENT_REQUEST"),
    };
    let plan = match plan(&request) {
        Ok(plan) => plan,
        Err(code) => return error(&code),
    };
    let generated = match crate::fixtures::generate(artifact_root, &request, attempt_key) {
        Some(result) => result.map_err(|_| "CANDIDATE_GENERATION_FAILED"),
        None => {
            generator::generate(artifact_root, &request, attempt_key).map_err(|error| error.code())
        }
    };
    match generated {
        Ok(mut result) => {
            result["development_plan"] = serde_json::to_value(plan).unwrap_or(Value::Null);
            result["development_request"] = request_value.clone();
            let accepted = match profile_acceptance::accept(artifact_root, args, &request, &result)
            {
                Ok(accepted) => accepted,
                Err(code) => return error(&code),
            };
            json!({
                "protocol_version": "external-harness-v1",
                "ok": true,
                "outcome": "succeeded",
                "result": accepted,
            })
        }
        Err(code) => error(code),
    }
}

pub fn plan(request: &DevelopmentRequest) -> Result<DevelopmentPlan, String> {
    let contracts = ContractCatalog::v1();
    contracts
        .validate_request(request)
        .map_err(|error| error.to_string())?;
    let profiles = ComponentProfileCatalog::v1();
    let profile = profiles
        .get(&request.build_profile)
        .ok_or_else(|| "UNKNOWN_COMPONENT_PROFILE".to_string())?;
    profile.validate_request(request)?;
    Ok(DevelopmentPlan {
        request_id: request.request_id.clone(),
        contract_catalog_version: contracts.version,
        component_profile_id: profile.profile_id.clone(),
        target_kind: request.target_kind,
        lifecycle_state: ComponentLifecycleState::Planned,
    })
}

pub fn discovery() -> Value {
    json!({
        "contract_catalog": ContractCatalog::v1(),
        "component_profiles": ComponentProfileCatalog::v1(),
    })
}

fn error(code: &str) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "outcome": "definitively_rejected",
        "error_code": code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
    use agent_core_kernel::domain::DevelopmentRequestDraft;

    fn request(kind: TargetKind, contract: &str, permission: &str) -> DevelopmentRequest {
        let mut draft = DevelopmentRequestDraft::new(kind, "external.example".into());
        draft.requirements = vec!["deliver an external component".into()];
        draft.required_contracts = vec![contract.into()];
        draft.requested_permissions = vec![permission.into()];
        draft.acceptance_criteria = vec!["profile gates pass".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "scope:test".into(),
            "message:test".into(),
            "development:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    #[test]
    fn hook_consumer_request_selects_profile_from_catalog() {
        let request = request(
            TargetKind::HookConsumerService,
            "event.observe.v0",
            "journal.observe",
        );
        let plan = plan(&request).unwrap();
        assert_eq!(plan.component_profile_id, "hook-consumer-service-v0");
        assert_eq!(plan.lifecycle_state, ComponentLifecycleState::Planned);
    }

    #[test]
    fn discovery_contains_contracts_and_all_profiles() {
        let value = discovery();
        assert_eq!(
            value["contract_catalog"]["version"],
            CONTRACT_CATALOG_VERSION
        );
        assert_eq!(
            value["component_profiles"]["profiles"]
                .as_array()
                .unwrap()
                .len(),
            7
        );
    }

    #[test]
    fn operation_errors_carry_the_generic_definitive_rejection_signal() {
        let value = error("ANY_BUSINESS_REASON");
        assert_eq!(value["ok"], false);
        assert_eq!(value["outcome"], "definitively_rejected");
        assert_eq!(value["error_code"], "ANY_BUSINESS_REASON");
    }
}
