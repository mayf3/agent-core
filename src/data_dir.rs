use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

const ROOT_MD: &str =
    "# Root System\n\nYou are the main Agent Core assistant. Keep Phase 0 chat-only.\n";
const RUNTIME_MD: &str = "# Runtime Contract\n\nExternal actions must be expressed as invocation intents and approved by Gateway.\n";
const AGENT_MD: &str =
    "# Main Agent\n\nDefault Phase 0 agent. It answers user messages without tools.\n";
const CHAT_SKILL_MD: &str = "# Chat\n\nReply clearly and briefly to the current user message.\n";

pub fn default_data_dir() -> PathBuf {
    home_dir()
        .map(|home| home.join(".agent-core"))
        .unwrap_or_else(|| PathBuf::from(".agent-core"))
}

pub fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(value));
    }
    PathBuf::from(value)
}

pub fn ensure_data_files(data_dir: &Path) -> Result<()> {
    write_if_missing(&data_dir.join("system/root.md"), ROOT_MD)?;
    write_if_missing(&data_dir.join("system/runtime.md"), RUNTIME_MD)?;
    write_if_missing(&data_dir.join("agents/main/AGENT.md"), AGENT_MD)?;
    write_if_missing(&data_dir.join("skills/chat/SKILL.md"), CHAT_SKILL_MD)?;
    Ok(())
}

pub fn copy_legacy_db_if_needed(legacy_path: &Path, new_path: &Path) -> Result<()> {
    if new_path.exists() || !legacy_path.exists() {
        return Ok(());
    }
    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(legacy_path, new_path)?;
    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
