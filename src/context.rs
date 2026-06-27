use crate::config::KernelConfig;
use crate::domain::*;
use crate::journal::JournalStore;
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

pub struct ContextAssembler {
    root_dir: PathBuf,
    recent_messages: usize,
    max_block_chars: usize,
}

impl ContextAssembler {
    pub fn from_config(config: &KernelConfig) -> Self {
        Self {
            root_dir: config.root_dir.clone(),
            recent_messages: config.context_recent_messages,
            max_block_chars: config.context_max_block_chars,
        }
    }

    pub fn build(
        &self,
        journal: &JournalStore,
        session: &Session,
        event: &ValidatedEvent,
        user_text: &str,
        granted_operations: &[String],
        snapshot: &RegistrySnapshot,
    ) -> Result<Vec<ContextBlock>> {
        let recent = self.recent_block(journal, session, &event.event_id.0)?;
        let mut blocks = vec![
            self.file_block(
                ContextBlockKind::RootSystem,
                "system/root.md",
                // Generic, safe fallback — NOT "Phase 0 chat-only". When the
                // external prompt file is absent/unreadable, the model is still
                // told it may use explicitly-provided, Gateway-authorized tools
                // and should prefer an authorized read-only tool over guessing.
                "You are the main Agent Core assistant. You may use tools that \
                 are explicitly provided in the current request and authorized \
                 by the Gateway. For real-time, system, or session facts, do \
                 not guess; prefer an authorized read-only tool. Never assume a \
                 tool that was not provided or not authorized.",
                Compressibility::Never,
            ),
            self.file_block(
                ContextBlockKind::RuntimeContract,
                "system/runtime.md",
                "External actions must be expressed as invocation intents and \
                 approved by Gateway. For real-time, system, or current-session \
                 facts, do not guess; use an authorized read-only tool if one is \
                 provided. Never assume a tool that was not provided.",
                Compressibility::Never,
            ),
            self.file_block(
                ContextBlockKind::AgentProfile,
                "agents/main/AGENT.md",
                "Main agent. You assist the user by answering messages and, when \
                 useful, calling the tools explicitly provided in the current \
                 request. Prefer an authorized read-only tool over guessing for \
                 real-time, system, or session facts. Do not assume tools that \
                 were not provided or not authorized.",
                Compressibility::Never,
            ),
            self.skill_catalog_block(),
            self.tool_catalog_block(granted_operations, snapshot),
            self.file_block(
                ContextBlockKind::ActiveSkill,
                "skills/chat/SKILL.md",
                "Reply clearly and briefly to the current user message.",
                Compressibility::DropWhole,
            ),
        ];
        if let Some(block) = recent {
            blocks.push(block);
        }
        blocks.push(block(
            ContextBlockKind::UserMessage,
            user_text,
            Compressibility::Truncate,
            &event.event_id.0,
            self.max_block_chars,
        ));
        Ok(blocks)
    }

    fn file_block(
        &self,
        kind: ContextBlockKind,
        relative_path: &str,
        fallback: &str,
        compressibility: Compressibility,
    ) -> ContextBlock {
        let content = read_text(&self.root_dir.join(relative_path)).unwrap_or(fallback.to_string());
        block(
            kind,
            &content,
            compressibility,
            relative_path,
            self.max_block_chars,
        )
    }

    fn skill_catalog_block(&self) -> ContextBlock {
        let content = skill_catalog(&self.root_dir)
            .unwrap_or_else(|| "chat: basic conversation skill".to_string());
        block(
            ContextBlockKind::SkillCatalog,
            &content,
            Compressibility::DropWhole,
            "skills/",
            self.max_block_chars,
        )
    }
    fn tool_catalog_block(
        &self,
        granted_operations: &[String],
        snapshot: &RegistrySnapshot,
    ) -> ContextBlock {
        let content = snapshot.catalog_for_context_grants(granted_operations);
        block(
            ContextBlockKind::ToolCatalog,
            &content,
            Compressibility::DropWhole,
            "operation/catalog",
            self.max_block_chars,
        )
    }

    fn recent_block(
        &self,
        journal: &JournalStore,
        session: &Session,
        current_event_id: &str,
    ) -> Result<Option<ContextBlock>> {
        let messages = journal
            .recent_user_messages(&session.id, self.recent_messages + 1)?
            .into_iter()
            .filter(|(event_id, _)| event_id != current_event_id)
            .map(|(_, text)| format!("User: {text}"))
            .collect::<Vec<_>>();
        if messages.is_empty() {
            return Ok(None);
        }
        Ok(Some(block(
            ContextBlockKind::RecentMessages,
            &messages.join("\n"),
            Compressibility::Summarizable,
            "journal/recent_messages",
            self.max_block_chars,
        )))
    }
}

fn block(
    kind: ContextBlockKind,
    content: &str,
    compressibility: Compressibility,
    source_ref: &str,
    max_chars: usize,
) -> ContextBlock {
    let content = if matches!(compressibility, Compressibility::Never) {
        content.trim().to_string()
    } else {
        truncate_chars(content.trim(), max_chars)
    };
    ContextBlock {
        kind,
        content,
        compressibility,
        source_ref: Some(source_ref.to_string()),
    }
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn skill_catalog(root_dir: &Path) -> Option<String> {
    let skills_dir = root_dir.join("skills");
    let mut rows = vec![];
    for entry in fs::read_dir(skills_dir).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let text = read_text(&entry.path().join("SKILL.md")).unwrap_or_default();
        rows.push(format!("{name}: {}", first_description(&text)));
    }
    if rows.is_empty() {
        return None;
    }
    rows.sort();
    Some(rows.join("\n"))
}

fn first_description(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("installed skill")
        .to_string()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 || text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut value = text
        .chars()
        .take(max_chars.saturating_sub(13))
        .collect::<String>();
    value.push_str("\n[truncated]");
    value
}
