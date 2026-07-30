use super::super::GenerationError;
use agent_core_kernel::registry::schema::{validate_against_schema, validate_schema_structure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GeneratedCapability {
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub probe_arguments: Value,
    pub probe_result: Value,
    pub contract_tests: Vec<ContractCase>,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapabilityContract {
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub probe_arguments: Value,
    pub probe_result: Value,
    pub contract_tests: Vec<ContractCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContractCase {
    pub case_id: String,
    pub arguments: Value,
    pub expected_result: Value,
}

impl GeneratedCapability {
    pub(super) fn parse(raw: &str) -> Result<(CapabilityContract, String), GenerationError> {
        let mut generated: Self = serde_json::from_str(raw)
            .map_err(|_| GenerationError::new("GENERATOR_MODEL_RESPONSE_INVALID"))?;
        generated.source = super::source::normalize(&generated.source)?;
        let contract = CapabilityContract {
            description: generated.description,
            input_schema: generated.input_schema,
            output_schema: generated.output_schema,
            probe_arguments: generated.probe_arguments,
            probe_result: generated.probe_result,
            contract_tests: generated.contract_tests,
        };
        contract.validate()?;
        Ok((contract, generated.source))
    }
}

impl CapabilityContract {
    pub(crate) fn load(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let contract: Self =
            serde_json::from_slice(&bytes).map_err(|_| "profile contract invalid".to_string())?;
        contract
            .validate()
            .map_err(|error| error.code().to_string())?;
        Ok(contract)
    }

    pub(super) fn validate(&self) -> Result<(), GenerationError> {
        if self.description.trim().is_empty()
            || self.description.len() > 512
            || self.description.chars().any(char::is_control)
        {
            return Err(invalid_contract());
        }
        validate_schema_structure(&self.input_schema).map_err(|_| invalid_schema())?;
        validate_schema_structure(&self.output_schema).map_err(|_| invalid_schema())?;
        if self.input_schema.get("type").and_then(Value::as_str) != Some("object")
            || self
                .input_schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                != Some(false)
            || self
                .output_schema
                .get("type")
                .and_then(Value::as_str)
                .is_none()
        {
            return Err(invalid_schema());
        }
        validate_against_schema(&self.input_schema, &self.probe_arguments)
            .map_err(|_| invalid_contract())?;
        validate_against_schema(&self.output_schema, &self.probe_result)
            .map_err(|_| invalid_contract())?;
        if self.contract_tests.len() < 2 || self.contract_tests.len() > 16 {
            return Err(invalid_contract());
        }
        let mut ids = BTreeSet::new();
        let mut inputs = BTreeSet::new();
        let mut probe_bound = false;
        for case in &self.contract_tests {
            if case.case_id.is_empty()
                || case.case_id.len() > 64
                || !case
                    .case_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                || !ids.insert(case.case_id.clone())
            {
                return Err(invalid_contract());
            }
            validate_against_schema(&self.input_schema, &case.arguments)
                .map_err(|_| invalid_contract())?;
            validate_against_schema(&self.output_schema, &case.expected_result)
                .map_err(|_| invalid_contract())?;
            let canonical =
                serde_json::to_string(&case.arguments).map_err(|_| invalid_contract())?;
            if !inputs.insert(canonical) {
                return Err(invalid_contract());
            }
            probe_bound |=
                case.arguments == self.probe_arguments && case.expected_result == self.probe_result;
        }
        if !probe_bound {
            return Err(invalid_contract());
        }
        Ok(())
    }
}

fn invalid_contract() -> GenerationError {
    GenerationError::new("GENERATOR_PROFILE_CONTRACT_INVALID")
}

fn invalid_schema() -> GenerationError {
    GenerationError::new("GENERATOR_SCHEMA_INVALID")
}
