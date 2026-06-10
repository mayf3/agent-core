use crate::domain::AgentId;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct KernelConfig {
    pub db_path: PathBuf,
    pub agent_id: AgentId,
    pub root_dir: PathBuf,
    pub kernel_port: u16,
    pub connector_execute_url: String,
    pub ipc_token: String,
    pub feishu_allowed_open_ids: Vec<String>,
    pub feishu_allowed_chat_ids: Vec<String>,
    pub feishu_require_group_mention: bool,
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub model: String,
    pub model_timeout_ms: u64,
}

impl KernelConfig {
    pub fn from_cli(db_path: Option<String>) -> Self {
        load_local_env();
        let root_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            db_path: db_path
                .map(PathBuf::from)
                .unwrap_or_else(|| root_dir.join(".agent-core/kernel.sqlite")),
            agent_id: AgentId("main".to_string()),
            root_dir,
            kernel_port: env_u16("AGENT_CORE_KERNEL_PORT", 4130),
            connector_execute_url: env_string(
                "AGENT_CORE_CONNECTOR_EXECUTE_URL",
                "http://127.0.0.1:4131/v1/execute",
            ),
            ipc_token: env_string("AGENT_CORE_IPC_TOKEN", ""),
            feishu_allowed_open_ids: env_list("AGENT_CORE_FEISHU_ALLOWED_OPEN_IDS"),
            feishu_allowed_chat_ids: env_list("AGENT_CORE_FEISHU_ALLOWED_CHAT_IDS"),
            feishu_require_group_mention: env_bool("AGENT_CORE_FEISHU_REQUIRE_GROUP_MENTION", true),
            openai_base_url: env_string("AGENT_CORE_OPENAI_BASE_URL", "https://api.openai.com/v1")
                .trim_end_matches('/')
                .to_string(),
            openai_api_key: env_string("AGENT_CORE_OPENAI_API_KEY", ""),
            model: env_string("AGENT_CORE_MODEL", ""),
            model_timeout_ms: env_u64("AGENT_CORE_MODEL_TIMEOUT_MS", 30_000),
        }
    }
}

fn load_local_env() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if std::env::var_os(key.trim()).is_some() {
            continue;
        }
        std::env::set_var(key.trim(), unquote(value.trim()));
    }
}

fn env_string(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|_| fallback.to_string())
        .trim()
        .to_string()
}

fn env_list(key: &str) -> Vec<String> {
    env_string(key, "")
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn env_bool(key: &str, fallback: bool) -> bool {
    std::env::var(key)
        .map(|value| value == "true")
        .unwrap_or(fallback)
}

fn env_u16(key: &str, fallback: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn env_u64(key: &str, fallback: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(value)
        .to_string()
}
