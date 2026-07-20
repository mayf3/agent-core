#[cfg(test)]
#[path = "../coding_private_origin_tests.rs"]
mod private_origin_tests;

#[cfg(test)]
mod component_manifest_tests {
    use crate::contract_catalog::CONTRACT_CATALOG_VERSION;
    use crate::domain::*;
    use serde_json::{json, Value};
    use super::super::invocable::invocable_manifest;

    fn request() -> DevelopmentRequest {
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.example".into(),
        );
        draft.requirements = vec!["provide a bounded invocation".into()];
        draft.required_contracts = vec!["component.invoke.v0".into()];
        draft.requested_permissions = vec!["component.invoke".into()];
        draft.acceptance_criteria = vec!["trusted contract tests pass".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "session:test".into(),
            "message:test".into(),
            "development:message:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    fn component() -> Value {
        json!({
            "schema_version": "component-artifact-v1",
            "component_id": "external.example",
            "kind": "invocable_capability",
            "profile_id": "invocable-capability-v0",
            "contract_catalog_version": CONTRACT_CATALOG_VERSION,
            "required_contracts": ["component.invoke.v0"],
            "requested_permissions": ["component.invoke"],
            "deployment_profile": "capability-host-v0",
            "capability": {
                "operation_name": "external.example",
                "description": "A bounded example capability.",
                "input_schema": {"type":"object","additionalProperties":false},
                "output_schema": {"type":"object"},
                "idempotent": true
            }
        })
    }

    #[test]
    fn post_gate_manifest_must_match_requested_contracts_and_permissions() {
        let request = request();
        let digest = format!("sha256:{}", "a".repeat(64));
        invocable_manifest(&request, &component(), &digest).unwrap();

        let mut escalated = component();
        escalated["requested_permissions"] = json!(["component.invoke", "deployment.effect"]);
        assert!(invocable_manifest(&request, &escalated, &digest).is_err());
    }
}
