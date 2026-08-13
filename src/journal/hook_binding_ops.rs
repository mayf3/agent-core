//! Online HookBinding bind / unbind operations on the Journal.
//!
//! Mirrors the `enable_harness` / `disable_harness` pattern exactly:
//!
//! ```text
//! current snapshot
//! → validate expected_snapshot_id
//! → load current snapshot
//! → clone operations + hook_bindings
//! → modify only the target contract's HookBinding
//! → create_registry_snapshot_with_hook_bindings
//! → activate_snapshot_transactional (CAS + RegistrySnapshotActivated event)
//! ```
//!
//! Existing Runs keep the snapshot they pinned at creation; only NEW Runs
//! pin the newly activated snapshot. No direct SQL writes to
//! `registry_state` outside the existing transactional activation.

use crate::domain::{ApprovedHookBindingChange, HookBindingChangeAction};
use crate::harness::control::RegistryActivationResult;
use crate::journal::JournalStore;
use crate::registry::snapshot::BindingKind;
use anyhow::{bail, Result};

impl JournalStore {
    /// Set (or replace) the HookBinding for `contract` in a NEW active
    /// snapshot. Replace semantics: at most one binding per contract; an
    /// existing binding for the same contract is replaced. Binding the
    /// identical binding is an idempotent no-op (`changed: false`).
    ///
    /// External bindings must carry a non-empty `provider_id` and `endpoint`.
    pub fn bind_hook(
        &self,
        approved: &ApprovedHookBindingChange,
    ) -> Result<RegistryActivationResult> {
        let intent = &approved.intent;
        if intent.action != HookBindingChangeAction::Bind {
            bail!("invalid_hook_binding_change: bind_hook requires Bind action");
        }
        let binding = intent
            .binding
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("invalid_hook_binding_change: binding required"))?;
        if binding.contract != intent.contract {
            bail!("invalid_hook_binding_change: binding contract mismatch");
        }
        if binding.binding_kind == BindingKind::External {
            if binding.provider_id.trim().is_empty() {
                bail!("invalid_hook_binding: external binding requires non-empty provider_id");
            }
            if binding.endpoint.trim().is_empty() {
                bail!("invalid_hook_binding: external binding requires non-empty endpoint");
            }
        }

        // Load current snapshot and verify the expected snapshot id.
        let current = self.current_registry_snapshot_id()?;
        if &current != &intent.expected_snapshot_id {
            bail!(
                "snapshot_conflict: expected {}, current {}",
                intent.expected_snapshot_id,
                current
            );
        }
        let current_snap = self.load_registry_snapshot(&current)?;

        // Clone bindings and replace (or append) the target contract's entry.
        let mut new_bindings: Vec<crate::registry::snapshot::HookBinding> =
            current_snap.hook_bindings.clone();
        match new_bindings.iter().position(|b| b.contract == intent.contract) {
            Some(idx) => {
                if new_bindings[idx] == *binding {
                    // Idempotent: identical binding already active.
                    return Ok(RegistryActivationResult {
                        previous_snapshot_id: current.clone(),
                        active_snapshot_id: current,
                        changed: false,
                    });
                }
                new_bindings[idx] = binding.clone();
            }
            None => new_bindings.push(binding.clone()),
        }

        // Only the target contract's binding changed; operations and all
        // other hook bindings are preserved verbatim.
        let new_snapshot = self.create_registry_snapshot_with_hook_bindings(
            current_snap.operations.clone(),
            new_bindings,
        )?;
        let new_snapshot_id = new_snapshot.snapshot_id.clone();

        // Atomically: CAS-update registry_state + RegistrySnapshotActivated
        // journal event + memory cache.
        self.activate_snapshot_transactional(
            &intent.expected_snapshot_id,
            &new_snapshot_id,
            &approved.decision_id,
            "hook_binding_bind",
        )?;

        Ok(RegistryActivationResult {
            previous_snapshot_id: current,
            active_snapshot_id: new_snapshot_id,
            changed: true,
        })
    }

    /// Remove the HookBinding for `contract` in a NEW active snapshot. All
    /// operations and the other hook bindings are preserved verbatim.
    /// Unbinding a contract that is not bound is an idempotent no-op
    /// (`changed: false`).
    pub fn unbind_hook(
        &self,
        approved: &ApprovedHookBindingChange,
    ) -> Result<RegistryActivationResult> {
        let intent = &approved.intent;
        if intent.action != HookBindingChangeAction::Unbind {
            bail!("invalid_hook_binding_change: unbind_hook requires Unbind action");
        }

        // Load current snapshot and verify the expected snapshot id.
        let current = self.current_registry_snapshot_id()?;
        if &current != &intent.expected_snapshot_id {
            bail!(
                "snapshot_conflict: expected {}, current {}",
                intent.expected_snapshot_id,
                current
            );
        }
        let current_snap = self.load_registry_snapshot(&current)?;

        if !current_snap
            .hook_bindings
            .iter()
            .any(|b| b.contract == intent.contract)
        {
            // Idempotent: contract not bound.
            return Ok(RegistryActivationResult {
                previous_snapshot_id: current.clone(),
                active_snapshot_id: current,
                changed: false,
            });
        }

        let new_bindings: Vec<crate::registry::snapshot::HookBinding> = current_snap
            .hook_bindings
            .iter()
            .filter(|b| b.contract != intent.contract)
            .cloned()
            .collect();

        let new_snapshot = self.create_registry_snapshot_with_hook_bindings(
            current_snap.operations.clone(),
            new_bindings,
        )?;
        let new_snapshot_id = new_snapshot.snapshot_id.clone();

        self.activate_snapshot_transactional(
            &intent.expected_snapshot_id,
            &new_snapshot_id,
            &approved.decision_id,
            "hook_binding_unbind",
        )?;

        Ok(RegistryActivationResult {
            previous_snapshot_id: current,
            active_snapshot_id: new_snapshot_id,
            changed: true,
        })
    }
}
