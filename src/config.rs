use crate::domain::AgentId;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct KernelConfig {
    pub db_path: PathBuf,
    pub agent_id: AgentId,
    pub root_dir: PathBuf,
}

impl KernelConfig {
    pub fn from_cli(db_path: Option<String>) -> Self {
        let root_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            db_path: db_path
                .map(PathBuf::from)
                .unwrap_or_else(|| root_dir.join(".agent-core/kernel.sqlite")),
            agent_id: AgentId("main".to_string()),
            root_dir,
        }
    }
}
