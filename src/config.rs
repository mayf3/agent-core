use crate::data_dir::{copy_legacy_db_if_needed, default_data_dir, ensure_data_files, expand_home};
use crate::domain::AgentId;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct KernelConfig {
    pub db_path: PathBuf,
    pub data_dir: PathBuf,
    pub agent_id: AgentId,
    pub root_dir: PathBuf,
    pub kernel_port: u16,
    pub connector_execute_url: String,
    pub ipc_token: String,
    /// Token for the capability submitter principal. Distinct from the
    /// decision token so submitter ≠ decision principal is enforced.
    pub capability_submit_token: String,
    /// Token for the external Approval Workflow principal that makes
    /// approve/reject decisions on capability change proposals.
    pub capability_decision_token: String,
    pub feishu_allowed_open_ids: Vec<String>,
    pub feishu_allowed_chat_ids: Vec<String>,
    pub feishu_require_group_mention: bool,
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub model: String,
    pub fallback_openai_base_url: String,
    pub fallback_openai_api_key: String,
    pub fallback_model: String,
    pub model_timeout_ms: u64,
    pub context_recent_messages: usize,
    pub context_max_block_chars: usize,
    pub outbox_dispatcher_enabled: bool,
    pub outbox_dispatcher_poll_interval_ms: u64,
    /// Extra operation names a run principal is granted in addition to its
    /// channel's baseline grant (Phase 2 M2b config-driven grants). Each name
    /// must be in the operation catalog; unknown names are dropped when the
    /// profile is derived. Empty (the default) ⇒ the principal receives only
    /// its channel's baseline grant, preserving prior behavior.
    pub extra_allowed_operations: Vec<String>,
    /// Phase 2 M2d opt-in: when true, a `risk: Write` operation pauses the
    /// run in `AwaitingApproval` until a human decision resumes it. Default
    /// false → all operations inline-approve (backward compatible).
    pub require_write_approval: bool,
    /// Phase 2 M2d follow-up: max seconds an `AwaitingApproval` run may
    /// wait for a human decision before startup recovery expires it to
    /// `Failed` (appends `ApprovalExpired`). 0 = disabled (default → a
    /// paused run waits indefinitely, the pre-expiry behavior). Only
    /// consulted when `require_write_approval` is true.
    pub write_approval_ttl_secs: u64,
    /// When true, the fallback LLM endpoint uses IndexedMapping for tool
    /// names (e.g. DeepSeek that rejects dots in function names).
    /// Configured via AGENT_CORE_FALLBACK_TOOL_NAME_INDEXED (default: false).
    pub fallback_tool_name_indexed: bool,
    /// When true, the primary LLM endpoint uses IndexedMapping for tool
    /// names.
    /// Configured via AGENT_CORE_PRIMARY_TOOL_NAME_INDEXED (default: false).
    pub primary_tool_name_indexed: bool,
    /// Maximum time to wait for a single external-harness HTTP response,
    /// in milliseconds. Default 10_000 (10s). A Run pinned to a snapshot
    /// containing an external operation uses this read timeout for the
    /// loopback harness transport; reaching it yields error_category
    /// `timeout` (never an unbounded hang). Configured via
    /// AGENT_CORE_HARNESS_READ_TIMEOUT_MS.
    pub harness_read_timeout_ms: u64,
}

impl KernelConfig {
    pub fn from_cli(db_path: Option<String>) -> Self {
        load_local_env();
        let workspace_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let data_dir = std::env::var("AGENT_CORE_DATA_DIR")
            .map(|value| expand_home(value.trim()))
            .unwrap_or_else(|_| default_data_dir());
        let root_dir = std::env::var("AGENT_CORE_CONTEXT_DIR")
            .map(|value| expand_home(value.trim()))
            .unwrap_or_else(|_| data_dir.clone());
        let default_db_path = data_dir.join("kernel.sqlite");
        let legacy_db_path = workspace_dir.join(".agent-core/kernel.sqlite");
        if db_path.is_none() {
            let _ = copy_legacy_db_if_needed(&legacy_db_path, &default_db_path);
        }
        match ensure_data_files(&data_dir) {
            Ok(report) if report.migration_needed > 0 => eprintln!(
                "bootstrap_prompt_migration_needed customized_files_preserved={}",
                report.migration_needed
            ),
            Ok(_) => {}
            Err(_) => eprintln!("bootstrap_prompt_setup_failed"),
        }
        Self {
            db_path: db_path.map(PathBuf::from).unwrap_or(default_db_path),
            data_dir,
            agent_id: AgentId("main".to_string()),
            root_dir,
            kernel_port: env_u16("AGENT_CORE_KERNEL_PORT", 4130),
            connector_execute_url: env_string(
                "AGENT_CORE_CONNECTOR_EXECUTE_URL",
                "http://127.0.0.1:4131/v1/execute",
            ),
            ipc_token: env_string("AGENT_CORE_IPC_TOKEN", ""),
            capability_submit_token: env_string("AGENT_CORE_CAPABILITY_SUBMIT_TOKEN", ""),
            capability_decision_token: env_string("AGENT_CORE_CAPABILITY_DECISION_TOKEN", ""),
            feishu_allowed_open_ids: env_list("AGENT_CORE_FEISHU_ALLOWED_OPEN_IDS"),
            feishu_allowed_chat_ids: env_list("AGENT_CORE_FEISHU_ALLOWED_CHAT_IDS"),
            feishu_require_group_mention: env_bool("AGENT_CORE_FEISHU_REQUIRE_GROUP_MENTION", true),
            openai_base_url: env_string("AGENT_CORE_OPENAI_BASE_URL", "https://api.openai.com/v1")
                .trim_end_matches('/')
                .to_string(),
            openai_api_key: env_string("AGENT_CORE_OPENAI_API_KEY", ""),
            model: env_string("AGENT_CORE_MODEL", ""),
            fallback_openai_base_url: env_string("AGENT_CORE_FALLBACK_OPENAI_BASE_URL", "")
                .trim_end_matches('/')
                .to_string(),
            fallback_openai_api_key: env_string("AGENT_CORE_FALLBACK_OPENAI_API_KEY", ""),
            fallback_model: env_string("AGENT_CORE_FALLBACK_MODEL", ""),
            model_timeout_ms: env_u64("AGENT_CORE_MODEL_TIMEOUT_MS", 30_000),
            context_recent_messages: env_usize("AGENT_CORE_CONTEXT_RECENT_MESSAGES", 6),
            context_max_block_chars: env_usize("AGENT_CORE_CONTEXT_MAX_BLOCK_CHARS", 4_000),
            outbox_dispatcher_enabled: env_bool("AGENT_CORE_OUTBOX_DISPATCHER_ENABLED", true),
            outbox_dispatcher_poll_interval_ms: env_u64(
                "AGENT_CORE_OUTBOX_DISPATCHER_POLL_MS",
                500,
            ),
            // system.status is part of the dogfood agent's profile (not a
            // channel grant, see ExecutionProfile::for_channel). It is granted
            // here in the default config so the dogfood agent can query system
            // health. Future agents must explicitly configure this grant via
            // extra_allowed_operations.
            extra_allowed_operations: {
                let mut ops = env_list("AGENT_CORE_EXTRA_ALLOWED_OPERATIONS");
                if !ops.contains(&"system.status".to_string()) {
                    ops.push("system.status".to_string());
                }
                ops
            },
            require_write_approval: env_bool("AGENT_CORE_REQUIRE_WRITE_APPROVAL", false),
            write_approval_ttl_secs: env_u64("AGENT_CORE_WRITE_APPROVAL_TTL_SECS", 0),
            fallback_tool_name_indexed: env_bool("AGENT_CORE_FALLBACK_TOOL_NAME_INDEXED", false),
            primary_tool_name_indexed: env_bool("AGENT_CORE_PRIMARY_TOOL_NAME_INDEXED", false),
            harness_read_timeout_ms: env_u64("AGENT_CORE_HARNESS_READ_TIMEOUT_MS", 10_000),
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

/// Parse a comma-separated env-var value into non-empty, trimmed strings.
/// Both production (`env_list`) and tests share this pure function so there
/// is exactly one split/trim/filter code path.
pub(crate) fn parse_env_list_value(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn env_list(key: &str) -> Vec<String> {
    parse_env_list_value(&env_string(key, ""))
}

/// Parse a boolean env-var value. Pure (no env access) so production and tests
/// share one code path. Accepted true values (case-insensitive, trimmed):
/// `true`, `1`, `yes`, `on`. Accepted false values: `false`, `0`, `no`, `off`.
/// Any other non-empty value is treated as a misconfiguration and falls back
/// to `fallback` (false) — a deployment that sets an invalid value does NOT
/// silently enable indexed mapping. `env_bool` logs nothing and never prints
/// the raw env value.
pub(crate) fn parse_env_bool_value(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err("invalid_boolean_config".to_string()),
    }
}

fn env_bool(key: &str, fallback: bool) -> bool {
    let raw = match std::env::var(key) {
        Ok(v) => v,
        Err(_) => return fallback,
    };
    match parse_env_bool_value(&raw) {
        Ok(v) => v,
        Err(_) => {
            // Invalid value — abort startup so the operator knows.
            eprintln!("invalid_boolean_config: {key}");
            std::process::exit(1);
        }
    }
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

fn env_usize(key: &str, fallback: usize) -> usize {
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
