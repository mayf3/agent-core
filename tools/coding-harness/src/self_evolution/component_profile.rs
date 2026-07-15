use agent_core_kernel::domain::{DevelopmentRequest, TargetKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentProfileCatalog {
    pub version: String,
    pub profiles: Vec<ComponentProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentProfile {
    pub profile_id: String,
    pub target_kinds: Vec<TargetKind>,
    pub project_shape: String,
    pub build: Vec<String>,
    pub gates: Vec<String>,
    pub sandbox: String,
    pub dependencies: Vec<String>,
    pub permissions: Vec<String>,
    pub supported_contracts: Vec<String>,
    pub artifact_manifest: String,
    pub deployment: String,
    pub healthcheck: String,
    pub rollback: String,
}

impl ComponentProfileCatalog {
    pub fn v1() -> Self {
        Self {
            version: "component-profile-catalog-v1".into(),
            profiles: vec![
                profile(
                    "invocable-capability-v0",
                    &[TargetKind::InvocableCapability],
                    "single process-harness-v1 executable",
                    &["cargo build --release --locked"],
                    &["component.invoke.v0"],
                    &["component.invoke"],
                    "capability-host content-addressed activation",
                    "trusted protocol invocation",
                ),
                profile(
                    "hook-consumer-service-v0",
                    &[TargetKind::HookConsumerService],
                    "long-running read-only HTTP service with durable cursor and rebuildable projection",
                    &["frozen dependency install", "build", "unit test", "package"],
                    &["event.observe.v0", "feishu.reply.v0"],
                    &["journal.observe", "feishu.reply"],
                    "deployment.effect.v0 managed service",
                    "HTTP readiness plus cursor/projection status",
                ),
                profile(
                    "context-provider-v0",
                    &[TargetKind::ContextProvider],
                    "external context provider service",
                    &["frozen dependency install", "build", "contract test", "package"],
                    &["context.load.v0"],
                    &["context.read"],
                    "managed external service",
                    "contract response and dependency readiness",
                ),
                profile(
                    "context-transformer-v0",
                    &[TargetKind::ContextTransformer],
                    "stateless external context transform service",
                    &["frozen dependency install", "build", "contract test", "package"],
                    &["context.prepare.v0", "context.compress.v0"],
                    &["context.transform"],
                    "managed external service",
                    "transform contract and provenance checks",
                ),
                profile(
                    "scheduled-worker-v0",
                    &[TargetKind::ScheduledWorker, TargetKind::SchedulerService],
                    "external scheduler or bounded worker",
                    &["frozen dependency install", "build", "replay test", "package"],
                    &["run.create.v0", "feishu.reply.v0"],
                    &["run.propose", "feishu.reply"],
                    "managed service with schedule state outside Kernel",
                    "heartbeat, last tick, and proposal receipt",
                ),
                profile(
                    "router-service-v0",
                    &[TargetKind::IngressRouter, TargetKind::ConnectorExtension],
                    "external propose-only router service",
                    &["frozen dependency install", "build", "route replay test", "package"],
                    &["route.proposal.v0", "feishu.reply.v0"],
                    &["route.propose", "feishu.reply"],
                    "managed external service",
                    "proposal validity and no direct effect path",
                ),
                profile(
                    "multi-run-orchestrator-v0",
                    &[TargetKind::MultiRunOrchestrator],
                    "external correlation and multi-run proposal service",
                    &["frozen dependency install", "build", "multi-run replay test", "package"],
                    &["run.create.v0", "component.invoke.v0"],
                    &["run.propose", "component.invoke"],
                    "managed external service",
                    "correlation progress and bounded fan-out",
                ),
            ],
        }
    }

    pub fn get(&self, profile_id: &str) -> Option<&ComponentProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.profile_id == profile_id)
    }
}

impl ComponentProfile {
    pub fn validate_request(&self, request: &DevelopmentRequest) -> Result<(), String> {
        if !self.target_kinds.contains(&request.target_kind) {
            return Err("COMPONENT_PROFILE_TARGET_KIND_MISMATCH".into());
        }
        for contract in &request.required_contracts {
            if !self.supported_contracts.contains(contract) {
                return Err(format!("COMPONENT_PROFILE_CONTRACT_UNSUPPORTED:{contract}"));
            }
        }
        for permission in &request.requested_permissions {
            if !self.permissions.contains(permission) {
                return Err(format!("COMPONENT_PROFILE_PERMISSION_DENIED:{permission}"));
            }
        }
        Ok(())
    }
}

fn profile(
    id: &str,
    kinds: &[TargetKind],
    shape: &str,
    build: &[&str],
    contracts: &[&str],
    permissions: &[&str],
    deployment: &str,
    healthcheck: &str,
) -> ComponentProfile {
    ComponentProfile {
        profile_id: id.into(),
        target_kinds: kinds.to_vec(),
        project_shape: shape.into(),
        build: build.iter().map(|value| value.to_string()).collect(),
        gates: [
            "scaffold",
            "build",
            "trusted_test",
            "trusted_smoke",
            "artifact",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        sandbox:
            "mandatory fail-closed process isolation; network denied unless the profile grants it"
                .into(),
        dependencies: vec!["locked and reproducible dependencies only".into()],
        permissions: permissions.iter().map(|value| value.to_string()).collect(),
        supported_contracts: contracts.iter().map(|value| value.to_string()).collect(),
        artifact_manifest: "component-artifact-v1".into(),
        deployment: deployment.into(),
        healthcheck: healthcheck.into(),
        rollback: "retain last-known-good digest and require a terminal rollback receipt".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_contains_the_seven_required_profiles() {
        let catalog = ComponentProfileCatalog::v1();
        for id in [
            "invocable-capability-v0",
            "hook-consumer-service-v0",
            "context-provider-v0",
            "context-transformer-v0",
            "scheduled-worker-v0",
            "router-service-v0",
            "multi-run-orchestrator-v0",
        ] {
            let profile = catalog.get(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(profile.gates.len(), 5);
            assert!(!profile.rollback.is_empty());
        }
    }
}
