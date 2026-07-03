//! Workspace configuration with per-workspace permissions.
//! Supports: read, write, exec, zcode, network, shell.
//! Defaults: network=false, shell=false.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct WorkspacePermission {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    pub zcode: bool,
    pub network: bool,
    pub shell: bool,
}

impl Default for WorkspacePermission {
    fn default() -> Self {
        Self {
            read: false,
            write: false,
            exec: false,
            zcode: false,
            network: false,
            shell: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceEntry {
    pub root: PathBuf,
    pub perm: WorkspacePermission,
}

#[derive(Debug, Clone)]
pub struct CodingConfig {
    pub workspaces: HashMap<String, WorkspaceEntry>,
}

impl CodingConfig {
    pub fn from_env() -> Self {
        let raw = match std::env::var("CODING_CONFIG") {
            Ok(v) => v,
            Err(_) => {
                return Self {
                    workspaces: HashMap::new(),
                }
            }
        };
        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("warning: CODING_CONFIG is not valid JSON");
                return Self {
                    workspaces: HashMap::new(),
                };
            }
        };
        let mut workspaces = HashMap::new();
        if let Some(wss) = parsed.get("workspaces").and_then(|v| v.as_object()) {
            for (id, cfg) in wss {
                let root_str = cfg.get("root").and_then(|v| v.as_str()).unwrap_or("");
                let canon =
                    std::fs::canonicalize(root_str).unwrap_or_else(|_| PathBuf::from(root_str));
                let perm = WorkspacePermission {
                    read: cfg.get("read").and_then(|v| v.as_bool()).unwrap_or(false),
                    write: cfg.get("write").and_then(|v| v.as_bool()).unwrap_or(false),
                    exec: cfg.get("exec").and_then(|v| v.as_bool()).unwrap_or(false),
                    zcode: cfg.get("zcode").and_then(|v| v.as_bool()).unwrap_or(false),
                    network: cfg
                        .get("network")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    shell: cfg.get("shell").and_then(|v| v.as_bool()).unwrap_or(false),
                };
                workspaces.insert(id.clone(), WorkspaceEntry { root: canon, perm });
            }
        }
        Self { workspaces }
    }

    pub fn root_for(&self, id: &str) -> Option<&PathBuf> {
        self.workspaces.get(id).map(|e| &e.root)
    }

    pub fn perm_for(&self, id: &str) -> Option<&WorkspacePermission> {
        self.workspaces.get(id).map(|e| &e.perm)
    }
}
