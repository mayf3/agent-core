//! Workspace configuration — operator-defined workspace_id → absolute_root_path mappings.

use std::collections::HashMap;
use std::path::PathBuf;

/// Operator-defined workspace roots. The model can only reference workspace_id
/// and relative paths within these roots.
#[derive(Debug, Clone)]
pub struct WorkspaceConfig {
    /// workspace_id → canonical absolute root path.
    pub workspaces: HashMap<String, PathBuf>,
    /// Additional env-var names to pass through to subprocesses (besides PATH,
    /// HOME, TMPDIR). The operator defines these statically; the model never
    /// supplies arbitrary env vars.
    pub exec_env_pass: Vec<String>,
}

impl WorkspaceConfig {
    /// Parse from the WORKSPACE_CONFIG JSON env var.
    /// Expected format:
    /// ```json
    /// {
    ///   "workspaces": { "id": "/abs/path", ... },
    ///   "exec_env_pass": ["VAR1", "VAR2"]
    /// }
    /// ```
    /// MISSING env var → empty config (all operations fail with unknown workspace).
    pub fn from_env() -> Self {
        let raw = match std::env::var("WORKSPACE_CONFIG") {
            Ok(v) => v,
            Err(_) => {
                return Self {
                    workspaces: HashMap::new(),
                    exec_env_pass: Vec::new(),
                };
            }
        };
        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("warning: WORKSPACE_CONFIG is not valid JSON");
                return Self {
                    workspaces: HashMap::new(),
                    exec_env_pass: Vec::new(),
                };
            }
        };
        let mut workspaces = HashMap::new();
        if let Some(wss) = parsed.get("workspaces").and_then(|v| v.as_object()) {
            for (id, path_val) in wss {
                if let Some(p) = path_val.as_str() {
                    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
                    workspaces.insert(id.clone(), canon);
                }
            }
        }
        let exec_env_pass = parsed
            .get("exec_env_pass")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Self {
            workspaces,
            exec_env_pass,
        }
    }

    /// Look up a workspace root by id. Returns None if unknown.
    pub fn root_for(&self, id: &str) -> Option<&PathBuf> {
        self.workspaces.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_env_returns_empty_config() {
        // Not setting WORKSPACE_CONFIG should yield empty config.
        // We unset the env within the test scope.
        let prev = std::env::var("WORKSPACE_CONFIG").ok();
        std::env::remove_var("WORKSPACE_CONFIG");
        let cfg = WorkspaceConfig::from_env();
        assert!(cfg.workspaces.is_empty());
        assert!(cfg.exec_env_pass.is_empty());
        if let Some(v) = prev {
            std::env::set_var("WORKSPACE_CONFIG", v);
        }
    }

    #[test]
    fn parse_valid_json() {
        let json = r#"{"workspaces":{"a":"/tmp/a"},"exec_env_pass":["MY_VAR"]}"#;
        let prev = std::env::var("WORKSPACE_CONFIG").ok();
        std::env::set_var("WORKSPACE_CONFIG", json);
        let cfg = WorkspaceConfig::from_env();
        assert!(cfg.workspaces.contains_key("a"));
        assert_eq!(cfg.exec_env_pass, vec!["MY_VAR"]);
        if let Some(v) = prev {
            std::env::set_var("WORKSPACE_CONFIG", v);
        } else {
            std::env::remove_var("WORKSPACE_CONFIG");
        }
    }

    #[test]
    fn invalid_json_returns_empty() {
        let prev = std::env::var("WORKSPACE_CONFIG").ok();
        std::env::set_var("WORKSPACE_CONFIG", "not json");
        let cfg = WorkspaceConfig::from_env();
        assert!(cfg.workspaces.is_empty());
        if let Some(v) = prev {
            std::env::set_var("WORKSPACE_CONFIG", v);
        } else {
            std::env::remove_var("WORKSPACE_CONFIG");
        }
    }
}
