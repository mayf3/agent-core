//! Shared test support for capability route decision tests. The helpers here
//! are `pub(super)` and reused by sibling modules.

use crate::capabilities::store::{ContentStore, Sha256Digest};
use crate::domain::*;
use crate::gateway::Gateway;
use crate::harness::manifest::HarnessManifest;
use crate::journal::JournalStore;
use crate::server::capability_routes::handle_submit_proposal;
use anyhow::Result;
use serde_json::{json, Value};

// ── Test support ───────────────────────────────────────────────────────────

/// A complete, valid setup for a single capability change proposal.
pub(super) struct ProposalSetup {
    pub(super) store: ContentStore,
    pub(super) artifact_digest: String,
    pub(super) manifest_digest: String,
    pub(super) evidence_digest: String,
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
        let body = json!({
            "target_agent_id": "agent_main",
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
            body,
        })
    }

    /// Submit the proposal via the real submit handler and return its id.
    pub(super) fn submit(&self, journal: &JournalStore, gateway: &Gateway) -> Result<String> {
        let resp = handle_submit_proposal(journal, gateway, &self.body, "capability_submitter")?;
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
        capability_submit_token: String::new(),
        capability_decision_token: String::new(),
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
