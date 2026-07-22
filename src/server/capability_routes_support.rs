//! Shared test support for capability route decision tests. The helpers here
//! are `pub(super)` and reused by sibling modules.

use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_submit_proposal;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

// ── Test support ───────────────────────────────────────────────────────────

/// A complete, valid setup for a single capability change proposal.
pub(super) struct ProposalSetup {
    pub(super) store: ContentStore,
    pub(super) artifact_digest: String,
    pub(super) manifest_digest: String,
    pub(super) evidence_digest: String,
    pub(super) manifest_id: String,
    pub(super) body: Value,
}

impl ProposalSetup {
    /// Build a valid setup. `op_name` is the operation the manifest exposes
    /// (and, by default, what the proposal requests). `requested_ops` overrides
    /// the proposal's requested operations when the test needs a mismatch.
    pub(super) fn build(
        op_name: &str,
        endpoint: &str,
        requested_ops: Option<Vec<String>>,
    ) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "cap_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir)?;
        let store = ContentStore::new(dir.join("store"));

        let artifact_bytes = b"#!/bin/sh\necho probe artifact\n";
        let artifact_digest = store.store(artifact_bytes)?;

        let evidence_bytes = br#"{"attestation":"test-build","signed_by":"ci"}"#;
        let evidence_digest = store.store(evidence_bytes)?;

        let mut manifest = HarnessManifest {
            manifest_id: String::new(),
            harness_id: "capability_probe_harness".into(),
            artifact_digest: artifact_digest.as_str().into(),
            protocol_version: "external-harness-v1".into(),
            endpoint: endpoint.into(),
            operation_name: op_name.into(),
            description: "Capability probe — read-only localhost health check.".into(),
            input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            output_schema: json!({"type":"object","properties":{"status":{"type":"string"},"ok":{"type":"boolean"}},"required":["status","ok"],"additionalProperties":false}),
            idempotent: true,
            created_at: chrono::Utc::now(),
        };
        manifest.manifest_id = manifest.compute_manifest_id()?;
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        let manifest_digest = store.store(&manifest_bytes)?;

        let requested = requested_ops.unwrap_or_else(|| vec![op_name.to_string()]);
        let mid = manifest.manifest_id.clone();
        let body = json!({
            "target_agent_id": "main",
            "artifact_ref": "artifact.bin",
            "artifact_digest": artifact_digest.as_str(),
            "manifest_ref": "manifest.json",
            "manifest_digest": manifest_digest.as_str(),
            "evidence_ref": "evidence.json",
            "evidence_digest": evidence_digest.as_str(),
            "requested_operations": requested,
            "risk_summary": "read-only localhost probe",
        });

        Ok(Self {
            store,
            artifact_digest: artifact_digest.as_str().to_string(),
            manifest_digest: manifest_digest.as_str().to_string(),
            evidence_digest: evidence_digest.as_str().to_string(),
            manifest_id: mid,
            body,
        })
    }

    /// Submit the proposal via the real submit handler and return its id.
    pub(super) fn submit(&self, journal: &JournalStore, gateway: &Gateway) -> Result<String> {
        let resp = handle_submit_proposal(
            journal,
            gateway,
            &self.body,
            "capability_submitter",
            &crate::domain::AgentId("main".to_string()),
        )?;
        Ok(resp.proposal_id)
    }

    /// A valid approved decision body (digests match the proposal) as JSON.
    pub(super) fn approved_body(&self) -> Value {
        json!({
            "decision": "approved",
            "artifact_digest": self.artifact_digest,
            "manifest_digest": self.manifest_digest,
        })
    }
}

/// A schema-only upgrade proposal setup. Given an already-active manifest
/// (`old`), produces a new manifest that varies only in the schema/description
/// fields by default, plus the content-store blobs and proposal/decision
/// bodies needed to drive `handle_submit_proposal` + `handle_decision` through
/// the schema-upgrade path.
///
/// The `mutate` closure allows a test to override specific fields on the new
/// manifest (e.g. change the endpoint) before its id is computed and stored;
/// when the closure is the identity, the result is a valid schema-only upgrade.
pub(super) struct SchemaUpgradeSetup {
    pub(super) store: ContentStore,
    pub(super) manifest: HarnessManifest,
    pub(super) manifest_digest: String,
    pub(super) body: Value,
}

impl SchemaUpgradeSetup {
    /// Build a schema-upgrade setup. `old` is the currently active manifest and
    /// `artifact_bytes` are the raw artifact bytes the original proposal stored
    /// (so this setup's own content-store can re-store them for verification).
    /// `new_description`/`new_input`/`new_output` override the schema/desc fields.
    /// `mutate` runs after those overrides and before the manifest_id is computed,
    /// so a test can deliberately break an immutable field (endpoint, harness_id,
    /// artifact_digest, protocol_version, idempotent) to exercise a rejection.
    pub(super) fn build(
        old: &HarnessManifest,
        artifact_bytes: &[u8],
        new_description: Option<&str>,
        new_input: Option<Value>,
        new_output: Option<Value>,
        mutate: Option<&dyn Fn(&mut HarnessManifest)>,
    ) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "cap_upgrade_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir)?;
        let store = ContentStore::new(dir.join("store"));

        // Re-store the original artifact bytes so the verifier can re-load them.
        let stored_art = store.store(artifact_bytes)?;
        // Sanity: the re-stored digest must equal the old artifact_digest, since
        // content addressing is deterministic. If a test passes mismatched bytes
        // we want to fail loudly here rather than in the handler.
        if stored_art.as_str() != old.artifact_digest {
            return Err(anyhow!(
                "artifact_bytes digest {} does not match old.artifact_digest {}",
                stored_art.as_str(),
                old.artifact_digest
            ));
        }
        let artifact_digest = old.artifact_digest.clone();

        let mut manifest = HarnessManifest {
            manifest_id: String::new(),
            harness_id: old.harness_id.clone(),
            artifact_digest: artifact_digest.clone(),
            protocol_version: old.protocol_version.clone(),
            endpoint: old.endpoint.clone(),
            operation_name: old.operation_name.clone(),
            description: new_description
                .map(String::from)
                .unwrap_or_else(|| format!("{} (schema upgrade)", old.description)),
            input_schema: new_input.unwrap_or_else(|| old.input_schema.clone()),
            output_schema: new_output.unwrap_or_else(|| old.output_schema.clone()),
            idempotent: old.idempotent,
            created_at: chrono::Utc::now(),
        };
        if let Some(f) = mutate {
            f(&mut manifest);
        }
        manifest.manifest_id = manifest.compute_manifest_id()?;
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        let manifest_digest = store.store(&manifest_bytes)?;
        let evidence_digest = store.store(br#"{"attestation":"schema-upgrade"}"#)?;

        let body = json!({
            "target_agent_id": "main",
            "artifact_ref": "artifact.bin",
            "artifact_digest": artifact_digest,
            "manifest_ref": "manifest.json",
            "manifest_digest": manifest_digest.as_str(),
            "evidence_ref": "evidence.json",
            "evidence_digest": evidence_digest.as_str(),
            "requested_operations": [manifest.operation_name],
            "risk_summary": "schema upgrade",
        });

        Ok(Self {
            store,
            manifest,
            manifest_digest: manifest_digest.as_str().to_string(),
            body,
        })
    }

    /// Submit the upgrade proposal via the real submit handler.
    pub(super) fn submit(&self, journal: &JournalStore, gateway: &Gateway) -> Result<String> {
        let resp = handle_submit_proposal(
            journal,
            gateway,
            &self.body,
            "capability_submitter",
            &crate::domain::AgentId("main".to_string()),
        )?;
        Ok(resp.proposal_id)
    }

    /// Approved decision body for this upgrade setup.
    pub(super) fn approved_body(&self) -> Value {
        json!({
            "decision": "approved",
            "artifact_digest": self.manifest.artifact_digest,
            "manifest_digest": self.manifest_digest,
        })
    }
}

pub(super) fn gateway() -> Gateway {
    use crate::config::KernelConfig;
    Gateway::new(KernelConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        data_dir: std::path::PathBuf::from(".agent-core-test"),
        agent_id: AgentId("main".to_string()),
        root_dir: std::path::PathBuf::from("."),
        kernel_port: 0,
        connector_execute_url: "http://127.0.0.1:0/v1/execute".to_string(),
        ipc_token: "test-token".to_string(),
        capability_submit_token: None,
        capability_decision_token: None,
        feishu_allowed_open_ids: vec![],
        feishu_allowed_chat_ids: vec![],
        feishu_require_group_mention: true,
        openai_base_url: "https://example.invalid/v1".to_string(),
        openai_api_key: String::new(),
        model: String::new(),
        fallback_openai_base_url: String::new(),
        fallback_openai_api_key: String::new(),
        fallback_model: String::new(),
        model_timeout_ms: 100,
        context_recent_messages: 6,
        context_max_block_chars: 4_000,
        outbox_dispatcher_enabled: false,
        outbox_dispatcher_poll_interval_ms: 100,
        extra_allowed_operations: vec!["system.status".to_string()],
        require_write_approval: false,
        write_approval_ttl_secs: 0,
        fallback_tool_name_indexed: false,
        primary_tool_name_indexed: false,
        harness_read_timeout_ms: 10_000,
        harness_artifact_root: std::env::temp_dir().join(format!("ha_root_{}", std::process::id())),
        coding_harness_api_url: "http://127.0.0.1:7200".into(),
        coding_harness_artifact_digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
        max_tool_rounds: 12,
        feishu_coding_owner_id: None,
        tool_loop_timeout_ms: 300_000,
        context_prepare_hook: crate::hook::HookConfig::default(),
    })
}

pub(super) const PROBE_OP: &str = "external.capability_probe";
pub(super) const ENDPOINT: &str = "http://127.0.0.1:18999/probe";

/// Count journal events of a given kind.
pub(super) fn count_events(journal: &JournalStore, kind: JournalEventKind) -> usize {
    journal
        .events()
        .unwrap_or_default()
        .iter()
        .filter(|e| e.kind == kind)
        .count()
}

/// The registry_state version (monotonic, incremented on each activation).
pub(super) fn registry_version(journal: &JournalStore) -> i64 {
    let conn = journal.conn.lock().unwrap();
    conn.query_row(
        "SELECT version FROM registry_state WHERE singleton_id = 1",
        [],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Overwrite the stored object bytes for `digest` in place, simulating
/// on-disk tampering. The next ContentStore::load will re-hash and reject.
pub(super) fn tamper_object(store: &ContentStore, digest_str: &str, bytes: &[u8]) -> Result<()> {
    let digest = Sha256Digest::parse(digest_str)?;
    std::fs::write(store.object_path(&digest), bytes)?;
    Ok(())
}

/// Return only the `RegistrySnapshotActivated` payloads whose `action` field
/// equals `schema_upgrade` (excludes plain `capability_activation` events).
pub(super) fn schema_upgrade_payloads(journal: &JournalStore) -> Vec<Value> {
    journal
        .events()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.kind == JournalEventKind::RegistrySnapshotActivated)
        .filter(|e| e.payload.get("action").and_then(|v| v.as_str()) == Some("schema_upgrade"))
        .map(|e| e.payload)
        .collect()
}

/// Number of manifest rows in `harness_manifests` for a given operation_name.
pub(super) fn manifest_count_for_operation(journal: &JournalStore, op_name: &str) -> i64 {
    let conn = journal.conn.lock().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM harness_manifests WHERE operation_name = ?1",
        [&op_name],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// True if a manifest row exists with the given manifest_id.
pub(super) fn manifest_exists(journal: &JournalStore, manifest_id: &str) -> bool {
    let conn = journal.conn.lock().unwrap();
    conn.query_row(
        "SELECT 1 FROM harness_manifests WHERE manifest_id = ?1",
        [&manifest_id],
        |row| row.get::<_, i64>(0),
    )
    .is_ok()
}

/// Insert a Run row pinned to `registry_snapshot_id` so a test can later
/// assert the Run's pinned snapshot is immutable across upgrades. Uses raw
/// SQL because the test does not need the full Run lifecycle.
pub(super) fn insert_pinned_run(journal: &JournalStore, run_id: &str, snapshot_id: &str) {
    let conn = journal.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO runs
         (id, session_id, agent_id, trigger_event_id, principal_json, parent_run_id, delegated_by,
          status, created_at, updated_at, registry_snapshot_id)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            run_id,
            "sess_test",
            "main",
            "evt_test",
            "{\"PrincipalId\":\"tester\"}",
            "Running",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            snapshot_id,
        ],
    )
    .unwrap();
}

/// Read the pinned `registry_snapshot_id` for a Run row by id.
pub(super) fn run_snapshot_id(journal: &JournalStore, run_id: &str) -> Option<String> {
    let conn = journal.conn.lock().unwrap();
    conn.query_row(
        "SELECT registry_snapshot_id FROM runs WHERE id = ?1",
        [&run_id],
        |row| row.get(0),
    )
    .ok()
    .flatten()
}
