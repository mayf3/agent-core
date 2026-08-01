use crate::domain::AgentId;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// File-backed Feishu chat_id → agent_id routing table.
///
/// Loaded from `data_dir/bindings/feishu.json`. Validation is fail-closed: an
/// invalid file is rejected with a descriptive `invalid_feishu_bindings` error
/// rather than silently falling back to the default agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeishuBindings {
    pub version: u32,
    pub bindings: Vec<FeishuBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeishuBinding {
    pub chat_id: String,
    pub agent_id: String,
}

impl FeishuBindings {
    /// Load bindings from `path`. A missing file yields `Ok(None)` (no routing
    /// configured → all chats fall back to the default agent). Any other I/O
    /// failure or invalid content yields `Err` (fail-closed).
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(anyhow!(
                    "failed to read feishu bindings at {}: {err}",
                    path.display()
                ))
            }
        };
        Ok(Some(Self::parse(&text)?))
    }

    /// Parse and validate bindings text (fail-closed).
    pub fn parse(text: &str) -> Result<Self> {
        let bindings: Self = serde_json::from_str(text)
            .map_err(|err| anyhow!("invalid_feishu_bindings: {err}"))?;
        bindings.validate()?;
        Ok(bindings)
    }

    /// Fail-closed validation: version must be 1, every entry must have
    /// non-empty `chat_id`/`agent_id`, and `chat_id` must be unique.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!(
                "invalid_feishu_bindings: unsupported version {}, expected 1",
                self.version
            );
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for binding in &self.bindings {
            if binding.chat_id.trim().is_empty() {
                bail!("invalid_feishu_bindings: empty chat_id");
            }
            if binding.agent_id.trim().is_empty() {
                bail!("invalid_feishu_bindings: empty agent_id");
            }
            if !seen.insert(&binding.chat_id) {
                bail!(
                    "invalid_feishu_bindings: duplicate chat_id {}",
                    binding.chat_id
                );
            }
        }
        Ok(())
    }

    /// Resolve `chat_id` to its bound agent. `None` when the chat is not bound
    /// (caller decides the fallback).
    pub fn resolve_agent_id(&self, chat_id: &str) -> Option<AgentId> {
        self.bindings
            .iter()
            .find(|binding| binding.chat_id == chat_id)
            .map(|binding| AgentId(binding.agent_id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const VALID: &str = r#"{
        "version": 1,
        "bindings": [
            { "chat_id": "oc_a", "agent_id": "worker-a" },
            { "chat_id": "oc_b", "agent_id": "main" }
        ]
    }"#;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agent-core-bindings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn valid_file_loads_and_resolves() {
        let dir = temp_dir();
        let path = dir.join("feishu.json");
        std::fs::write(&path, VALID).unwrap();
        let bindings = FeishuBindings::load(&path).unwrap().unwrap();
        assert_eq!(bindings.resolve_agent_id("oc_a"), Some(AgentId("worker-a".into())));
        assert_eq!(bindings.resolve_agent_id("oc_b"), Some(AgentId("main".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_none() {
        let dir = temp_dir();
        let path = dir.join("not-there.json");
        assert!(FeishuBindings::load(&path).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_fails_closed() {
        let err = FeishuBindings::parse("{ not json").unwrap_err().to_string();
        assert!(err.contains("invalid_feishu_bindings"), "err: {err}");
    }

    #[test]
    fn wrong_version_fails_closed() {
        let err = FeishuBindings::parse(
            r#"{"version":2,"bindings":[{"chat_id":"oc_a","agent_id":"worker-a"}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unsupported version"), "err: {err}");
    }

    #[test]
    fn missing_field_fails_closed() {
        let err = FeishuBindings::parse(r#"{"version":1,"bindings":[{"chat_id":"oc_a"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid_feishu_bindings"), "err: {err}");
    }

    #[test]
    fn empty_chat_id_fails_closed() {
        let err = FeishuBindings::parse(
            r#"{"version":1,"bindings":[{"chat_id":"","agent_id":"worker-a"}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("empty chat_id"), "err: {err}");
    }

    #[test]
    fn empty_agent_id_fails_closed() {
        let err = FeishuBindings::parse(
            r#"{"version":1,"bindings":[{"chat_id":"oc_a","agent_id":""}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("empty agent_id"), "err: {err}");
    }

    #[test]
    fn duplicate_chat_id_fails_closed() {
        let err = FeishuBindings::parse(
            r#"{"version":1,"bindings":[
                {"chat_id":"oc_a","agent_id":"worker-a"},
                {"chat_id":"oc_a","agent_id":"main"}
            ]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate chat_id"), "err: {err}");
    }

    #[test]
    fn unknown_fields_fail_closed() {
        let err = FeishuBindings::parse(
            r#"{"version":1,"bindings":[],"extra":true}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid_feishu_bindings"), "err: {err}");
    }

    #[test]
    fn empty_bindings_list_is_valid_and_resolves_nothing() {
        let bindings = FeishuBindings::parse(r#"{"version":1,"bindings":[]}"#).unwrap();
        assert_eq!(bindings.resolve_agent_id("oc_a"), None);
    }

    #[test]
    fn unbound_chat_resolves_none() {
        let bindings = FeishuBindings::parse(VALID).unwrap();
        assert_eq!(bindings.resolve_agent_id("oc_unknown"), None);
    }

    #[test]
    fn io_error_other_than_not_found_is_err() {
        let dir = temp_dir();
        // A directory at the target path makes read_to_string fail with a
        // non-NotFound error.
        let path = dir.join("feishu.json");
        std::fs::create_dir_all(&path).unwrap();
        assert!(FeishuBindings::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
