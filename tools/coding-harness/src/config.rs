use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct WorkspacePermission {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    pub opencode: bool,
    pub network: bool,
    pub shell: bool,
}

impl Default for WorkspacePermission {
    fn default() -> Self {
        Self {
            read: false,
            write: false,
            exec: false,
            opencode: false,
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
    /// Kernel API URL for capability proposal submission (e.g. http://127.0.0.1:4130)
    pub kernel_api_url: String,
    /// Submit token for POST /v1/capability-change-proposals
    pub capability_submit_token: String,
    /// Content store root for artifact/manifest/evidence blobs
    pub artifact_root: PathBuf,
}

impl CodingConfig {
    pub fn from_env() -> Self {
        let raw = std::env::var("CODING_CONFIG").unwrap_or_default();
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);

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
                    opencode: cfg
                        .get("opencode")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    network: cfg
                        .get("network")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    shell: cfg.get("shell").and_then(|v| v.as_bool()).unwrap_or(false),
                };
                workspaces.insert(id.clone(), WorkspaceEntry { root: canon, perm });
            }
        }

        Self {
            workspaces,
            kernel_api_url: std::env::var("KERNEL_API_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:4130".into()),
            capability_submit_token: std::env::var("CAPABILITY_SUBMIT_TOKEN").unwrap_or_default(),
            artifact_root: std::env::var("HARNESS_ARTIFACT_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir().join("coding-harness-artifacts")),
        }
    }

    pub fn root_for(&self, id: &str) -> Option<&PathBuf> {
        self.workspaces.get(id).map(|e| &e.root)
    }

    pub fn perm_for(&self, id: &str) -> Option<&WorkspacePermission> {
        self.workspaces.get(id).map(|e| &e.perm)
    }
}
