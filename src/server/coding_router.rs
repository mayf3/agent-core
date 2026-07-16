//! Compatibility router for Generic DevelopmentRequest V1.
//!
//! The router recognizes a development verb plus Contract Catalog references,
//! derives a component kind/profile, and emits a data-only draft. It does not
//! execute code or mint Kernel intents. The old calculator sentence is kept as
//! a fixture adapter and follows the same generic request path.

use crate::contract_catalog::ContractCatalog;
use crate::domain::{DevelopmentRequestDraft, TargetKind};
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingIntentKind {
    DevelopComponent,
}

#[derive(Debug, Clone)]
pub struct CodingIntent {
    pub kind: CodingIntentKind,
    pub development_request: DevelopmentRequestDraft,
    /// Compatibility fields used only by the frozen calculator E2E.
    pub operation: String,
    pub functions: Vec<String>,
    pub schema_version: String,
}

pub fn parse_coding_intent(text: &str) -> Result<CodingIntent> {
    let normalized = normalize(text)?;
    if contains_shell_pattern(&normalized) {
        bail!("SHELL_LIKE_REQUEST_REJECTED");
    }
    if contains_override_attempt(&normalized) {
        bail!("OPERATION_OVERRIDE_REJECTED");
    }
    if !contains_development_verb(&normalized) {
        bail!("UNSUPPORTED_CODING_REQUEST");
    }

    if is_calculator_fixture_request(&normalized) {
        let mut draft = DevelopmentRequestDraft::new(
            TargetKind::InvocableCapability,
            "external.calculator".to_string(),
        );
        draft.requirements = vec![
            "provide add, subtract, multiply, and divide operations".to_string(),
            "implement the component.invoke.v0 process contract".to_string(),
        ];
        draft.required_contracts = vec!["component.invoke.v0".to_string()];
        draft.requested_permissions = vec!["component.invoke".to_string()];
        draft.acceptance_criteria = vec![
            "trusted calculator fixture tests pass".to_string(),
            "multiply 6 by 7 returns 42".to_string(),
            "divide by zero returns a structured error".to_string(),
        ];
        return Ok(CodingIntent {
            kind: CodingIntentKind::DevelopComponent,
            development_request: draft,
            operation: "external.calculator".to_string(),
            functions: ["add", "subtract", "multiply", "divide"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            schema_version: "calculator-fixture-v0".to_string(),
        });
    }

    let catalog = ContractCatalog::v1();
    let required_contracts: Vec<String> = catalog
        .contracts
        .iter()
        .filter(|contract| normalized.contains(&contract.contract_id))
        .map(|contract| contract.contract_id.clone())
        .collect();
    if required_contracts.is_empty() {
        bail!("DEVELOPMENT_REQUEST_REQUIRES_KNOWN_CONTRACT");
    }
    let target_kind = infer_target_kind(&normalized, &required_contracts)?;
    let name = extract_component_name(text);
    let mut draft = DevelopmentRequestDraft::new(target_kind, name.clone());
    draft.requirements = split_requirements(text);
    draft.acceptance_criteria = draft.requirements.clone();
    draft.required_contracts = required_contracts;
    draft.requested_permissions = permissions_for(&catalog, &draft.required_contracts);
    draft.acceptance_kit_ref = kit_for_component(&name).map(str::to_string);

    Ok(CodingIntent {
        kind: CodingIntentKind::DevelopComponent,
        development_request: draft,
        operation: name,
        functions: Vec::new(),
        schema_version: target_kind.component_profile().to_string(),
    })
}

fn normalize(text: &str) -> Result<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("EMPTY_REQUEST");
    }
    Ok(trimmed.to_lowercase())
}

fn infer_target_kind(text: &str, contracts: &[String]) -> Result<TargetKind> {
    let has = |id: &str| contracts.iter().any(|value| value == id);
    if has("event.observe.v0") {
        return Ok(TargetKind::HookConsumerService);
    }
    if has("context.compress.v0") || has("context.prepare.v0") {
        return Ok(TargetKind::ContextTransformer);
    }
    if has("context.load.v0") {
        return Ok(TargetKind::ContextProvider);
    }
    if has("route.proposal.v0") {
        return Ok(TargetKind::IngressRouter);
    }
    if has("run.create.v0") {
        if text.contains("multi") || text.contains("多个") || text.contains("多 run") {
            return Ok(TargetKind::MultiRunOrchestrator);
        }
        if text.contains("scheduler") || text.contains("调度服务") {
            return Ok(TargetKind::SchedulerService);
        }
        return Ok(TargetKind::ScheduledWorker);
    }
    if has("feishu.reply.v0") {
        return Ok(TargetKind::ConnectorExtension);
    }
    if has("component.invoke.v0") {
        return Ok(TargetKind::InvocableCapability);
    }
    if has("deployment.effect.v0") {
        return Ok(TargetKind::HookConsumerService);
    }
    bail!("DEVELOPMENT_REQUEST_TARGET_KIND_AMBIGUOUS")
}

fn permissions_for(catalog: &ContractCatalog, contracts: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for id in contracts {
        if let Some(contract) = catalog.get(id) {
            for permission in &contract.permissions {
                if !values.contains(permission) {
                    values.push(permission.clone());
                }
            }
        }
    }
    values
}

fn extract_component_name(text: &str) -> String {
    let lower = text.to_lowercase();
    let prefixes = ["开发一个", "创建一个", "develop a", "create a"];
    let mut candidate = text.trim();
    for prefix in prefixes {
        if let Some(index) = lower.find(prefix) {
            candidate = &text[index + prefix.len()..];
            break;
        }
    }
    candidate = candidate
        .split(['，', ',', '\n', '。'])
        .next()
        .unwrap_or(candidate)
        .trim();
    if let Some(external) = candidate
        .split_whitespace()
        .find(|word| word.starts_with("external."))
    {
        return external
            .trim_matches(|value: char| {
                !value.is_ascii_alphanumeric() && value != '.' && value != '_'
            })
            .to_lowercase();
    }
    let mut slug = String::new();
    let mut separator = false;
    for byte in candidate.bytes() {
        if byte.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push((byte as char).to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    let slug = slug.trim_matches('-');
    if !slug.is_empty() {
        return slug.to_string();
    }
    let digest = hex::encode(Sha256::digest(candidate.as_bytes()));
    format!("component-{}", &digest[..16])
}

fn split_requirements(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    for item in text.split(['，', ',', '\n', '。']) {
        let value = item.trim();
        if !value.is_empty() && !values.iter().any(|existing| existing == value) {
            values.push(value.to_string());
        }
    }
    if values.is_empty() {
        values.push(text.trim().to_string());
    }
    values
}

fn contains_development_verb(text: &str) -> bool {
    ["开发", "创建", "develop", "create"]
        .iter()
        .any(|value| text.contains(value))
}

fn is_calculator_fixture_request(text: &str) -> bool {
    let calculator = text.contains("calculator") || text.contains("计算器");
    let arithmetic = [
        "加减乘除",
        "四则",
        "add",
        "subtract",
        "multiply",
        "divide",
        "arithmetic",
    ]
    .iter()
    .any(|value| text.contains(value));
    calculator && arithmetic
}

fn contains_shell_pattern(text: &str) -> bool {
    [
        "`", "$(", "; ", "| ", "&&", "||", "> ", ">> ", "rm ", "sudo ", "chmod ", "chown ",
        "wget ", "curl ", "/bin/", "/usr/", "/etc/", "~/.", "../",
    ]
    .iter()
    .any(|value| text.contains(value))
}

fn contains_override_attempt(text: &str) -> bool {
    ["operation=", "operation:", "coding_task_submit"]
        .iter()
        .any(|value| text.contains(value))
}

/// Map a component name to an explicit Acceptance Kit reference.
/// Uses exact name matching only — never substring matching on "token".
/// Returns None for components without a dedicated kit (the harness will
/// return ACCEPTANCE_KIT_SELECTION_REQUIRED).
fn kit_for_component(name: &str) -> Option<&'static str> {
    match name {
        "token-dashboard" => Some("token-dashboard-v0"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculator_is_a_compatibility_fixture_on_the_generic_profile() {
        let intent = parse_coding_intent("开发一个 external.calculator，支持加减乘除").unwrap();
        assert_eq!(intent.kind, CodingIntentKind::DevelopComponent);
        assert_eq!(intent.operation, "external.calculator");
        assert_eq!(
            intent.development_request.target_kind,
            TargetKind::InvocableCapability
        );
        assert_eq!(
            intent.development_request.build_profile,
            "invocable-capability-v0"
        );
        assert_eq!(intent.schema_version, "calculator-fixture-v0");
    }

    #[test]
    fn event_observer_request_selects_hook_consumer_profile() {
        let intent = parse_coding_intent(
            "开发一个 Token 使用量 Dashboard，\n通过 event.observe.v0 获取数据，\n按日期、Run、模型和 Profile 展示 Token 用量。",
        )
        .unwrap();
        let draft = intent.development_request;
        assert_eq!(draft.target_kind, TargetKind::HookConsumerService);
        assert_eq!(draft.name, "token-dashboard");
        assert_eq!(draft.build_profile, "hook-consumer-service-v0");
        assert_eq!(draft.deployment_profile, "managed-service-v0");
        assert_eq!(draft.required_contracts, ["event.observe.v0"]);
        assert_eq!(draft.requested_permissions, ["journal.observe"]);
    }

    #[test]
    fn unsupported_request_without_a_known_contract_is_rejected() {
        for text in ["开发一个浏览器", "create a web server"] {
            assert!(parse_coding_intent(text).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn shell_and_control_operation_overrides_are_rejected() {
        assert!(parse_coding_intent("开发一个 calculator && sudo ls").is_err());
        assert!(parse_coding_intent("开发一个 external.coding_task_submit").is_err());
    }

    #[test]
    fn empty_request_is_rejected() {
        assert!(parse_coding_intent("").is_err());
        assert!(parse_coding_intent("   ").is_err());
    }
}
