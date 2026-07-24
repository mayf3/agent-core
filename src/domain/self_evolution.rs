use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    InvocableCapability,
    HookConsumerService,
    ContextProvider,
    ContextTransformer,
    ScheduledWorker,
    SchedulerService,
    IngressRouter,
    MultiRunOrchestrator,
    ConnectorExtension,
}

impl TargetKind {
    pub fn component_profile(self) -> &'static str {
        match self {
            Self::InvocableCapability => "invocable-capability-v0",
            Self::HookConsumerService => "hook-consumer-service-v0",
            Self::ContextProvider => "context-provider-v0",
            Self::ContextTransformer => "context-transformer-v0",
            Self::ScheduledWorker | Self::SchedulerService => "scheduled-worker-v0",
            Self::IngressRouter | Self::ConnectorExtension => "router-service-v0",
            Self::MultiRunOrchestrator => "multi-run-orchestrator-v0",
        }
    }

    pub fn deployment_profile(self) -> &'static str {
        match self {
            Self::InvocableCapability => "capability-host-v0",
            _ => "managed-service-v0",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentRequest {
    #[serde(default)]
    pub request_id: String,
    pub source_subject: String,
    pub source_scope: String,
    pub source_message_id: String,
    pub target_kind: TargetKind,
    pub name: String,
    pub requirements: Vec<String>,
    pub required_contracts: Vec<String>,
    pub requested_permissions: Vec<String>,
    pub build_profile: String,
    pub deployment_profile: String,
    pub acceptance_criteria: Vec<String>,
    pub idempotency_key: String,
    pub contract_catalog_version: String,
}

impl DevelopmentRequest {
    pub fn from_draft(
        draft: DevelopmentRequestDraft,
        source_subject: String,
        source_scope: String,
        source_message_id: String,
        idempotency_key: String,
        contract_catalog_version: String,
    ) -> Result<Self> {
        let request = Self {
            request_id: String::new(),
            source_subject,
            source_scope,
            source_message_id,
            target_kind: draft.target_kind,
            name: draft.name,
            requirements: draft.requirements,
            required_contracts: draft.required_contracts,
            requested_permissions: draft.requested_permissions,
            build_profile: draft.build_profile,
            deployment_profile: draft.deployment_profile,
            acceptance_criteria: draft.acceptance_criteria,
            idempotency_key,
            contract_catalog_version,
        };
        request.with_derived_request_id()
    }

    /// Derive the content-addressed identity when an untrusted draft omits it.
    /// A caller-supplied identity is never replaced: mismatches still fail.
    pub fn with_derived_request_id(mut self) -> Result<Self> {
        if self.request_id.is_empty() {
            self.validate_body()?;
            self.request_id = self.derived_request_id()?;
        }
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_body()?;
        if self.request_id.is_empty() || self.request_id != self.derived_request_id()? {
            bail!("DEVELOPMENT_REQUEST_ID_MISMATCH");
        }
        Ok(())
    }

    fn validate_body(&self) -> Result<()> {
        for (field, value) in [
            ("source_subject", self.source_subject.as_str()),
            ("source_scope", self.source_scope.as_str()),
            ("source_message_id", self.source_message_id.as_str()),
            ("name", self.name.as_str()),
            ("build_profile", self.build_profile.as_str()),
            ("deployment_profile", self.deployment_profile.as_str()),
            ("idempotency_key", self.idempotency_key.as_str()),
            (
                "contract_catalog_version",
                self.contract_catalog_version.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                bail!("DEVELOPMENT_REQUEST_MISSING_{field}");
            }
        }
        if !is_safe_name(&self.name) {
            bail!("DEVELOPMENT_REQUEST_INVALID_NAME");
        }
        if self.requirements.is_empty() || self.acceptance_criteria.is_empty() {
            bail!("DEVELOPMENT_REQUEST_MISSING_REQUIREMENTS");
        }
        if self.build_profile != self.target_kind.component_profile()
            || self.deployment_profile != self.target_kind.deployment_profile()
        {
            bail!("DEVELOPMENT_REQUEST_PROFILE_MISMATCH");
        }
        ensure_nonempty_unique(&self.required_contracts, "REQUIRED_CONTRACTS")?;
        ensure_nonempty_unique(&self.requested_permissions, "REQUESTED_PERMISSIONS")?;
        ensure_nonempty_unique(&self.requirements, "REQUIREMENTS")?;
        ensure_nonempty_unique(&self.acceptance_criteria, "ACCEPTANCE_CRITERIA")?;
        Ok(())
    }

    fn derived_request_id(&self) -> Result<String> {
        let mut canonical_request = self.clone();
        canonical_request.request_id.clear();
        let canonical = serde_json::to_vec(&canonical_request)?;
        Ok(format!("devreq_{}", hex::encode(Sha256::digest(canonical))))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevelopmentRequestDraft {
    pub target_kind: TargetKind,
    pub name: String,
    pub requirements: Vec<String>,
    pub required_contracts: Vec<String>,
    pub requested_permissions: Vec<String>,
    pub build_profile: String,
    pub deployment_profile: String,
    pub acceptance_criteria: Vec<String>,
}

impl DevelopmentRequestDraft {
    pub fn new(target_kind: TargetKind, name: String) -> Self {
        Self {
            target_kind,
            name,
            requirements: Vec::new(),
            required_contracts: Vec::new(),
            requested_permissions: Vec::new(),
            build_profile: target_kind.component_profile().to_string(),
            deployment_profile: target_kind.deployment_profile().to_string(),
            acceptance_criteria: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveGapProposal {
    pub requested_feature: String,
    pub attempted_external_derivation: Vec<String>,
    pub available_contracts: Vec<String>,
    pub missing_fact_or_boundary: String,
    pub proof_of_blockage: Vec<String>,
    pub proposed_minimal_primitive: String,
    pub security_impact: String,
    pub compatibility_impact: String,
    pub tests: Vec<String>,
    pub resume_token: String,
}

impl PrimitiveGapProposal {
    pub fn validate(&self) -> Result<()> {
        if self.requested_feature.trim().is_empty()
            || self.missing_fact_or_boundary.trim().is_empty()
            || self.proof_of_blockage.is_empty()
            || self.proposed_minimal_primitive.trim().is_empty()
            || self.security_impact.trim().is_empty()
            || self.compatibility_impact.trim().is_empty()
            || self.tests.is_empty()
            || self.resume_token.trim().is_empty()
        {
            bail!("INVALID_PRIMITIVE_GAP_PROPOSAL");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepairClassification {
    ComponentBug,
    ContractMismatch,
    MissingPrimitive,
    InfrastructureFailure,
    ConfigurationError,
    PermissionDenied,
    DataCorruption,
    DependencyUnavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairRequest {
    pub component_id: String,
    pub observed_failure: String,
    pub evidence_refs: Vec<String>,
    pub classification: RepairClassification,
    pub reproduction: Vec<String>,
    pub current_artifact_digest: String,
    pub requested_fix: String,
    pub risk_delta: Value,
    pub rollback_target: String,
}

impl RepairRequest {
    pub fn validate(&self) -> Result<()> {
        if !is_safe_name(&self.component_id)
            || self.observed_failure.trim().is_empty()
            || self.evidence_refs.is_empty()
            || self.reproduction.is_empty()
            || !is_sha256(&self.current_artifact_digest)
            || self.requested_fix.trim().is_empty()
            || self.rollback_target.trim().is_empty()
        {
            bail!("INVALID_REPAIR_REQUEST");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentLifecycleState {
    Planned,
    Candidate,
    Accepted,
    Proposed,
    Approved,
    Deploying,
    Healthy,
    Disabled,
    RolledBack,
    Failed,
}

impl ComponentLifecycleState {
    pub fn can_transition_to(self, next: Self) -> bool {
        use ComponentLifecycleState::*;
        matches!(
            (self, next),
            (Planned, Candidate)
                | (Candidate, Accepted | Failed)
                | (Accepted, Proposed)
                | (Proposed, Approved | Failed)
                | (Approved, Deploying)
                | (Deploying, Healthy | Failed | RolledBack)
                | (Healthy, Disabled | Deploying | RolledBack | Failed)
                | (Disabled, Deploying | RolledBack)
                | (Failed, Candidate | RolledBack)
        )
    }
}

fn ensure_nonempty_unique(values: &[String], field: &str) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    if values.is_empty()
        || values.iter().any(|value| value.trim().is_empty())
        || values.iter().any(|value| !seen.insert(value))
    {
        bail!("DEVELOPMENT_REQUEST_INVALID_{field}");
    }
    Ok(())
}

fn is_safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> DevelopmentRequest {
        let mut draft =
            DevelopmentRequestDraft::new(TargetKind::HookConsumerService, "token-dashboard".into());
        draft.requirements = vec!["consume durable model usage facts".into()];
        draft.required_contracts = vec!["event.observe.v0".into()];
        draft.requested_permissions = vec!["journal.observe".into()];
        draft.acceptance_criteria = vec!["projection is rebuildable".into()];
        DevelopmentRequest::from_draft(
            draft,
            "feishu:open_id:owner".into(),
            "session:owner".into(),
            "message:1".into(),
            "development:message:1".into(),
            "contract-catalog-v1".into(),
        )
        .unwrap()
    }

    #[test]
    fn all_target_kinds_map_to_a_profile() {
        let kinds = [
            TargetKind::InvocableCapability,
            TargetKind::HookConsumerService,
            TargetKind::ContextProvider,
            TargetKind::ContextTransformer,
            TargetKind::ScheduledWorker,
            TargetKind::SchedulerService,
            TargetKind::IngressRouter,
            TargetKind::MultiRunOrchestrator,
            TargetKind::ConnectorExtension,
        ];
        assert!(kinds
            .iter()
            .all(|kind| !kind.component_profile().is_empty()));
    }

    #[test]
    fn lifecycle_requires_governed_order() {
        assert!(
            ComponentLifecycleState::Planned.can_transition_to(ComponentLifecycleState::Candidate)
        );
        assert!(
            !ComponentLifecycleState::Planned.can_transition_to(ComponentLifecycleState::Healthy)
        );
        assert!(
            ComponentLifecycleState::Healthy.can_transition_to(ComponentLifecycleState::RolledBack)
        );
    }

    #[test]
    fn development_request_id_is_deterministic_and_complete() {
        let first = request();
        let second = request();
        assert_eq!(first, second);
        assert!(first.request_id.starts_with("devreq_"));
        assert_eq!(first.target_kind, TargetKind::HookConsumerService);
        assert_eq!(first.build_profile, "hook-consumer-service-v0");
        first.validate().unwrap();
    }

    /// Same request payload must produce the same digest regardless of when
    /// or from where the request is constructed.
    #[test]
    fn same_request_same_digest_different_absolute_path() {
        // The DevelopmentRequest struct does not contain path information,
        // so any two requests with identical fields get identical digests.
        // We verify this explicitly: constructing the same request in
        // separate process-like contexts (different call paths, same data).
        let req_a = request();
        let req_b = request();
        assert_eq!(
            req_a.request_id, req_b.request_id,
            "identical requests must have identical digests"
        );
    }

    /// Changing the build profile must produce a different digest.
    #[test]
    fn build_profile_change_alters_digest() {
        let base = request(); // valid request with hook-consumer-service-v0 profile
        let mut modified = request();
        // Modify a field that's part of the digest but bypass the profile
        // validation by setting it after construction (we test the digest
        // sensitivity, not the validity of the modified request).
        modified.build_profile = "different-profile-v0".into();
        let base_digest = base.derived_request_id().unwrap();
        let modified_digest = modified.derived_request_id().unwrap();
        assert_ne!(
            base_digest, modified_digest,
            "different build_profile must produce different digest"
        );
    }

    /// Changing the required permissions must produce a different digest.
    #[test]
    fn permission_change_alters_digest() {
        let base = request();
        let mut modified = request();
        modified.requested_permissions = vec!["journal.observe".into(), "extra.permission".into()];
        let base_digest = base.derived_request_id().unwrap();
        let modified_digest = modified.derived_request_id().unwrap();
        assert_ne!(
            base_digest, modified_digest,
            "different permissions must produce different digest"
        );
    }

    /// Changing the contract catalog version (analogous to runtime version)
    /// must produce a different digest.
    #[test]
    fn contract_catalog_version_change_alters_digest() {
        let base = request();
        let mut modified = request();
        modified.contract_catalog_version = "contract-catalog-v2".into();
        let base_digest = base.derived_request_id().unwrap();
        let modified_digest = modified.derived_request_id().unwrap();
        assert_ne!(
            base_digest, modified_digest,
            "different contract_catalog_version must produce different digest"
        );
    }

    #[test]
    fn development_request_id_tampering_is_rejected() {
        let mut request = request();
        request.name = "different-component".into();
        assert!(request.validate().is_err());
    }

    #[test]
    fn primitive_gap_requires_proof_tests_and_resume_token() {
        let gap = PrimitiveGapProposal {
            requested_feature: "observe provider quota".into(),
            attempted_external_derivation: vec!["event.observe.v0".into()],
            available_contracts: vec!["event.observe.v0".into()],
            missing_fact_or_boundary: "provider quota is not a durable fact".into(),
            proof_of_blockage: vec!["replay contains no quota fact".into()],
            proposed_minimal_primitive: "provider.quota.observed.v0 fact".into(),
            security_impact: "read-scoped fact only".into(),
            compatibility_impact: "additive event".into(),
            tests: vec!["unknown fields remain observable".into()],
            resume_token: "resume:quota-dashboard:1".into(),
        };
        gap.validate().unwrap();
        let mut invalid = gap;
        invalid.resume_token.clear();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn repair_request_uses_the_fixed_failure_taxonomy() {
        let repair = RepairRequest {
            component_id: "token-dashboard".into(),
            observed_failure: "cursor stopped".into(),
            evidence_refs: vec!["event:123".into()],
            classification: RepairClassification::ComponentBug,
            reproduction: vec!["restart after page boundary".into()],
            current_artifact_digest:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            requested_fix: "persist cursor after projection commit".into(),
            risk_delta: serde_json::json!({"permissions_added": []}),
            rollback_target: "sha256:previous".into(),
        };
        repair.validate().unwrap();
    }
}
