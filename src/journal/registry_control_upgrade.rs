//! Restart-safe seeding of the governed Coding Harness control operations.

use crate::registry::snapshot::RegistrySnapshot;
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
        if expected
            .iter()
            .all(|spec| current.lookup(&spec.name) == Some(spec))
        {
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
