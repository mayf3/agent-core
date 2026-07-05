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
    /// Token for the capability submitter principal. `None` means the
    /// capability change routes are disabled (fail-closed). Must differ from
    /// `capability_decision_token` and `ipc_token`. Configured via
    /// AGENT_CORE_CAPABILITY_SUBMIT_TOKEN.
    pub capability_submit_token: Option<String>,
    /// Token for the external Approval Workflow principal that makes
    /// approve/reject decisions on capability change proposals. `None` means
    /// the decision routes are disabled (fail-closed). Must differ from
    /// `capability_submit_token` and `ipc_token`. Configured via
    /// AGENT_CORE_CAPABILITY_DECISION_TOKEN.
    pub capability_decision_token: Option<String>,
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
    /// Root directory of the content-addressed store holding capability
    /// change proposal blobs (artifact / manifest / evidence). The Kernel
    /// reads and re-hashes these bytes during decision verification — never
    /// trusts the submitter's claimed digest. Defaults to
    /// `<data_dir>/harness-artifacts`. Configured via
    /// AGENT_CORE_HARNESS_ARTIFACT_ROOT.
    pub harness_artifact_root: PathBuf,
    /// Maximum number of tool-call rounds per Run (each round is one LLM
    /// invocation that returns one or more tool calls). When the model
    /// reaches this limit without producing a final reply, the Run completes
    /// with ToolBudgetExhausted. Range 1–64, default 12. Configured via
    /// AGENT_CORE_MAX_TOOL_ROUNDS. A value outside the range causes a startup
    /// error (process exits with diagnostic).
    pub max_tool_rounds: usize,
    /// The Feishu open_id of the user authorized to use coding harness
    /// operations. When set, only private chats from this user receive
    /// the seven `external.coding_*` capability grants. Other principals
    /// (non-owner, group chats, CLI) do not receive coding grants.
    /// Default empty (no owner configured → no coding grants granted).
    /// Configured via AGENT_CORE_FEISHU_CODING_OWNER_ID.
    pub feishu_coding_owner_id: Option<String>,
    /// Maximum wall-clock time for the entire tool-call recall loop, in
    /// milliseconds. When this timeout is exceeded, the loop stops and
    /// emits a `ToolLoopWallClockExceeded` journal event. Default 300,000
    /// (5 minutes). Configured via AGENT_CORE_TOOL_LOOP_TIMEOUT_MS.
    pub tool_loop_timeout_ms: u64,
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
        let harness_artifact_root = std::env::var("AGENT_CORE_HARNESS_ARTIFACT_ROOT")
            .map(|v| expand_home(v.trim()))
            .unwrap_or_else(|_| data_dir.join("harness-artifacts"));
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
            capability_submit_token: env_optional_string("AGENT_CORE_CAPABILITY_SUBMIT_TOKEN"),
            capability_decision_token: env_optional_string("AGENT_CORE_CAPABILITY_DECISION_TOKEN"),
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
            harness_artifact_root,
            max_tool_rounds: env_max_tool_rounds("AGENT_CORE_MAX_TOOL_ROUNDS", 12),
            feishu_coding_owner_id: env_optional_string("AGENT_CORE_FEISHU_CODING_OWNER_ID"),
            tool_loop_timeout_ms: env_tool_loop_timeout_ms(
                "AGENT_CORE_TOOL_LOOP_TIMEOUT_MS",
                300_000,
            ),
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

fn env_optional_string(key: &str) -> Option<String> {
    let val = std::env::var(key).ok()?;
    let trimmed = val.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
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

/// Minimum allowed tool-loop wall-clock timeout in milliseconds.
pub(crate) const TOOL_LOOP_TIMEOUT_MS_MIN: u64 = 1_000;
/// Maximum allowed tool-loop wall-clock timeout in milliseconds (10 minutes).
pub(crate) const TOOL_LOOP_TIMEOUT_MS_MAX: u64 = 600_000;

/// Parse and validate a tool-loop timeout value. Pure function — no env access,
/// no process exit — so it is testable in isolation. Returns a descriptive error
/// for invalid inputs.
pub(crate) fn parse_tool_loop_timeout_ms(raw: &str) -> Result<u64, String> {
    let parsed: u64 = raw
        .parse()
        .map_err(|_| format!("not a valid integer, got {raw:?}"))?;
    if parsed < TOOL_LOOP_TIMEOUT_MS_MIN {
        return Err(format!(
            "must be at least {}, got {parsed}",
            TOOL_LOOP_TIMEOUT_MS_MIN
        ));
    }
    if parsed > TOOL_LOOP_TIMEOUT_MS_MAX {
        return Err(format!(
            "must be at most {}, got {parsed}",
            TOOL_LOOP_TIMEOUT_MS_MAX
        ));
    }
    Ok(parsed)
}

fn env_tool_loop_timeout_ms(key: &str, fallback: u64) -> u64 {
    let raw = match std::env::var(key) {
        Ok(v) => v,
        Err(_) => return fallback,
    };
    match parse_tool_loop_timeout_ms(&raw) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("invalid_config: {key} {msg}");
            std::process::exit(1);
        }
    }
}

fn env_max_tool_rounds(key: &str, fallback: usize) -> usize {
    let raw = match std::env::var(key) {
        Ok(v) => v,
        Err(_) => return fallback,
    };
    let parsed: usize = match raw.parse() {
        Ok(n) => n,
        Err(_) => {
            eprintln!("invalid_config: {key} must be an integer between 1 and 64, got {raw:?}");
            std::process::exit(1);
        }
    };
    if parsed < 1 || parsed > 64 {
        eprintln!("invalid_config: {key} must be between 1 and 64, got {parsed}");
        std::process::exit(1);
    }
    parsed
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_loop_timeout_uses_default_when_unset() {
        // env_tool_loop_timeout_ms returns fallback when env var is not set.
        // We cannot un-set an env var reliably in a multi-threaded test, so we
        // test the pure function path: parse_tool_loop_timeout_ms is exercised
        // in the other tests; env_tool_loop_timeout_ms fallback is exercised
        // here by never setting the key.
        let result = env_tool_loop_timeout_ms(
            "AGENT_CORE_TOOL_LOOP_TIMEOUT_TEST_MUST_NOT_EXIST_12345",
            300_000,
        );
        assert_eq!(result, 300_000);
    }

    #[test]
    fn tool_loop_timeout_accepts_valid_value() {
        let result = parse_tool_loop_timeout_ms("300000");
        assert_eq!(result, Ok(300_000));
    }

    #[test]
    fn tool_loop_timeout_accepts_minimum() {
        let result = parse_tool_loop_timeout_ms("1000");
        assert_eq!(result, Ok(1_000));
    }

    #[test]
    fn tool_loop_timeout_accepts_maximum() {
        let result = parse_tool_loop_timeout_ms("600000");
        assert_eq!(result, Ok(600_000));
    }

    #[test]
    fn tool_loop_timeout_rejects_non_numeric_value() {
        let result = parse_tool_loop_timeout_ms("abc");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("not a valid integer"), "msg: {msg}");
    }

    #[test]
    fn tool_loop_timeout_rejects_zero() {
        let result = parse_tool_loop_timeout_ms("0");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("must be at least"), "msg: {msg}");
    }

    #[test]
    fn tool_loop_timeout_rejects_above_maximum() {
        let result = parse_tool_loop_timeout_ms("999999");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("must be at most"), "msg: {msg}");
    }

    #[test]
    fn tool_loop_timeout_accepts_default_value() {
        let result = parse_tool_loop_timeout_ms("300000");
        assert_eq!(result, Ok(300_000));
    }
}
