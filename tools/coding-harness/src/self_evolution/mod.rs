pub mod acceptance_kit;
pub mod acceptance_selector;
pub mod artifact_manifest;
mod component_profile;
mod generator;

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
    let generated = match crate::fixtures::generate(artifact_root, &request) {
        Some(result) => result.map_err(|_| "CANDIDATE_GENERATION_FAILED"),
        None => generator::generate(artifact_root, &request).map_err(|error| error.code()),
    };
    match generated {
        Ok(mut result) => {
            result["development_plan"] = serde_json::to_value(plan).unwrap_or(Value::Null);
            result["development_request"] = request_value.clone();
            json!({
                "protocol_version": "external-harness-v1",
                "ok": true,
                "result": result,
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
}
