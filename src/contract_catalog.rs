use crate::domain::self_evolution::DevelopmentRequest;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

pub const CONTRACT_CATALOG_VERSION: &str = "contract-catalog-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractCatalog {
    pub version: String,
    pub contracts: Vec<ContractDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractDescriptor {
    pub contract_id: String,
    pub mode: ContractMode,
    pub schema: Value,
    pub permissions: Vec<String>,
    pub examples: Vec<Value>,
    pub sdk_bindings: Vec<String>,
    pub test_kit: String,
    pub compatibility_version: String,
    pub lifecycle: String,
    pub health_semantics: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractMode {
    Observe,
    Propose,
    Transform,
    Effect,
}

impl ContractCatalog {
    pub fn v1() -> Self {
        Self {
            version: CONTRACT_CATALOG_VERSION.to_string(),
            contracts: vec![
                contract(
                    "event.observe.v0",
                    ContractMode::Observe,
                    json!({"request":{"cursor":"string?","limit":"1..1000","filters":"object?"},"response":{"events":"array","next_cursor":"string","has_more":"boolean","schema_version":"event-observe-v0"}}),
                    &["journal.observe"],
                    json!({"cursor":null,"limit":100,"filters":{"kinds":["model.invocation.completed.v0"]}}),
                    "event-observe-v0-kit",
                    "append-only facts; unknown event fields are preserved",
                ),
                contract(
                    "context.prepare.v0",
                    ContractMode::Transform,
                    json!({"request":{"run_id":"string","blocks":"array"},"response":{"fragments":"array"}}),
                    &["context.transform"],
                    json!({"run_id":"run_example","blocks":[]}),
                    "context-prepare-v0-kit",
                    "healthy responses preserve required context blocks",
                ),
                contract(
                    "context.load.v0",
                    ContractMode::Observe,
                    json!({"request":{"scope":"string","refs":"array"},"response":{"blocks":"array"}}),
                    &["context.read"],
                    json!({"scope":"session:example","refs":[]}),
                    "context-load-v0-kit",
                    "unavailable sources are explicit and never silently fabricated",
                ),
                contract(
                    "context.compress.v0",
                    ContractMode::Transform,
                    json!({"request":{"blocks":"array","budget":"integer"},"response":{"blocks":"array","provenance":"array"}}),
                    &["context.transform"],
                    json!({"blocks":[],"budget":4096}),
                    "context-compress-v0-kit",
                    "compressed blocks retain provenance and budget accounting",
                ),
                contract(
                    "route.proposal.v0",
                    ContractMode::Propose,
                    json!({"request":{"source_event":"object"},"response":{"route":"string","confidence":"number","evidence":"array"}}),
                    &["route.propose"],
                    json!({"source_event":{"event_id":"event_example"}}),
                    "route-proposal-v0-kit",
                    "a route is only a proposal; Kernel validation remains authoritative",
                ),
                contract(
                    "run.create.v0",
                    ContractMode::Propose,
                    json!({"request":{"scope":"string","profile":"string","trigger":"object"},"response":{"run_id":"string","snapshot_id":"string"}}),
                    &["run.propose"],
                    json!({"scope":"session:example","profile":"default","trigger":{}}),
                    "run-create-v0-kit",
                    "created runs pin an immutable registry snapshot",
                ),
                contract(
                    "component.invoke.v0",
                    ContractMode::Effect,
                    json!({"request":{"component_id":"string","arguments":"object","idempotency_key":"string?"},"response":{"receipt":"object"}}),
                    &["component.invoke"],
                    json!({"component_id":"external.example","arguments":{}}),
                    "component-invoke-v0-kit",
                    "every effect is bound to an Allow Decision and one receipt",
                ),
                contract(
                    "deployment.effect.v0",
                    ContractMode::Effect,
                    json!({"request":{"intent_id":"string","artifact_digest":"sha256","expected_version":"string","action":"install|upgrade|disable|rollback"},"response":{"receipt":"object"}}),
                    &["deployment.effect"],
                    json!({"intent_id":"intent_example","artifact_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","expected_version":"1.0.0","action":"install"}),
                    "deployment-effect-v0-kit",
                    "terminal receipts include health and rollback evidence",
                ),
                contract(
                    "feishu.reply.v0",
                    ContractMode::Effect,
                    json!({"request":{"chat_id":"string","reply_to_message_id":"string?","presentation":"object"},"response":{"receipt":"object"}}),
                    &["feishu.reply"],
                    json!({"chat_id":"oc_example","presentation":{"kind":"text","text":"ok"}}),
                    "feishu-reply-v0-kit",
                    "delivery receipts are idempotent and never expose connector secrets",
                ),
            ],
        }
    }

    pub fn get(&self, contract_id: &str) -> Option<&ContractDescriptor> {
        self.contracts
            .iter()
            .find(|contract| contract.contract_id == contract_id)
    }

    pub fn validate_request(&self, request: &DevelopmentRequest) -> Result<()> {
        request.validate()?;
        if request.contract_catalog_version != self.version {
            bail!("CONTRACT_CATALOG_VERSION_MISMATCH");
        }
        for contract_id in &request.required_contracts {
            let contract = self
                .get(contract_id)
                .ok_or_else(|| anyhow::anyhow!("UNKNOWN_REQUIRED_CONTRACT:{contract_id}"))?;
            for permission in &contract.permissions {
                if !request.requested_permissions.contains(permission) {
                    bail!("MISSING_CONTRACT_PERMISSION:{permission}");
                }
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != CONTRACT_CATALOG_VERSION || self.contracts.is_empty() {
            bail!("INVALID_CONTRACT_CATALOG");
        }
        let mut ids = HashSet::new();
        for contract in &self.contracts {
            if !ids.insert(&contract.contract_id)
                || contract.schema.is_null()
                || contract.permissions.is_empty()
                || contract.examples.is_empty()
                || contract.sdk_bindings.is_empty()
                || contract.test_kit.is_empty()
                || contract.compatibility_version.is_empty()
                || contract.lifecycle.is_empty()
                || contract.health_semantics.is_empty()
            {
                bail!("INVALID_CONTRACT_DESCRIPTOR:{}", contract.contract_id);
            }
        }
        Ok(())
    }
}

fn contract(
    id: &str,
    mode: ContractMode,
    schema: Value,
    permissions: &[&str],
    example: Value,
    test_kit: &str,
    health: &str,
) -> ContractDescriptor {
    ContractDescriptor {
        contract_id: id.to_string(),
        mode,
        schema,
        permissions: permissions.iter().map(|value| value.to_string()).collect(),
        examples: vec![example],
        sdk_bindings: vec!["json-schema".to_string(), "rust-serde".to_string()],
        test_kit: test_kit.to_string(),
        compatibility_version: "v0".to_string(),
        lifecycle: "additive; incompatible changes require a new contract id".to_string(),
        health_semantics: health.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_v1_contracts_are_discoverable_and_complete() {
        let catalog = ContractCatalog::v1();
        catalog.validate().unwrap();
        for id in [
            "event.observe.v0",
            "context.prepare.v0",
            "context.load.v0",
            "context.compress.v0",
            "route.proposal.v0",
            "run.create.v0",
            "component.invoke.v0",
            "deployment.effect.v0",
            "feishu.reply.v0",
        ] {
            assert!(catalog.get(id).is_some(), "missing {id}");
        }
    }
}
