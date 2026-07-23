//! Restart-safe seeding of the governed Coding Harness control operations.

use crate::registry::snapshot::{OperationSpec, RegistrySnapshot};
use crate::registry::store::builtin_specs;
use anyhow::Result;
use std::sync::Arc;

impl super::JournalStore {
    pub(crate) fn ensure_coding_control_operations(
        &self,
        current: &Arc<RegistrySnapshot>,
    ) -> Result<bool> {
        let required = [
            crate::domain::operation::external::TASK_SUBMIT,
            crate::domain::operation::external::HCR_ACCEPT,
        ];
        let baseline = builtin_specs();
        let expected: Vec<_> = required
            .iter()
            .map(|name| {
                baseline
                    .iter()
                    .find(|spec| spec.name == *name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("coding_control_spec_missing"))
            })
            .collect::<Result<_>>()?;
        if expected.iter().all(|spec| {
            current
                .lookup(&spec.name)
                .is_some_and(|active| governed_control_spec_matches(active, spec))
        }) {
            return Ok(false);
        }
        // An older deployment may already have a general-purpose operation
        // with the same name. Replace it with the current governed control spec;
        // retaining the old binding/schema would silently bypass request and
        // acceptance bindings.
        let mut specs: Vec<_> = current
            .operations
            .iter()
            .filter(|spec| !required.contains(&spec.name.as_str()))
            .cloned()
            .collect();
        specs.extend(expected);
        let next = self.create_registry_snapshot(specs)?;
        self.activate_snapshot_transactional(
            &current.snapshot_id,
            &next.snapshot_id,
            &format!("seed_coding_controls:{}", current.snapshot_id),
            "seed_coding_control_operations",
        )?;
        Ok(true)
    }
}

fn governed_control_spec_matches(active: &OperationSpec, expected: &OperationSpec) -> bool {
    active.name == expected.name
        && active.risk == expected.risk
        && active.description == expected.description
        && active.parameters == expected.parameters
        && active.idempotent == expected.idempotent
        && active.binding_kind == expected.binding_kind
        && !active.binding_key.is_empty()
}

#[cfg(test)]
mod tests {
    use super::governed_control_spec_matches;
    use crate::registry::store::builtin_specs;

    #[test]
    fn content_addressed_manifest_binding_does_not_reseed_control_schema() {
        let expected = builtin_specs()
            .into_iter()
            .find(|spec| spec.name == crate::domain::operation::external::TASK_SUBMIT)
            .unwrap();
        let mut active = expected.clone();
        active.binding_key = format!("manifest_{}", "a".repeat(64));
        assert!(governed_control_spec_matches(&active, &expected));
        active.parameters = serde_json::json!({"type":"object"});
        assert!(!governed_control_spec_matches(&active, &expected));
    }
}
