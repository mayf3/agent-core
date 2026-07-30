use crate::config::KernelConfig;
use crate::domain::*;
use crate::journal::JournalStore;
use crate::registry::snapshot::RegistrySnapshot;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

pub struct ContextAssembler {
    root_dir: PathBuf,
}

impl ContextAssembler {
    pub fn from_config(config: &KernelConfig) -> Self {
        Self {
            root_dir: config.root_dir.clone(),
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
            ),
            self.file_block(
                ContextBlockKind::RuntimeContract,
                "system/runtime.md",
                "External actions must be expressed as invocation intents and \
                 approved by Gateway. For real-time, system, or current-session \
                 facts, do not guess; use an authorized read-only tool if one is \
                 provided. Never assume a tool that was not provided.",
            ),
            self.file_block(
                ContextBlockKind::AgentProfile,
                "agents/main/AGENT.md",
                "Main agent. You assist the user by answering messages and, when \
                 useful, calling the tools explicitly provided in the current \
                 request. Prefer an authorized read-only tool over guessing for \
                 real-time, system, or session facts. Do not assume tools that \
                 were not provided or not authorized.",
            ),
            self.skill_catalog_block(),
            self.tool_catalog_block(granted_operations, snapshot),
            self.file_block(
                ContextBlockKind::ActiveSkill,
                "skills/chat/SKILL.md",
                "Reply clearly and briefly to the current user message.",
            ),
        ];
        let turns = journal.conversation_turns(&session.id, Some(&event.event_id.0))?;
        if !turns.is_empty() {
            let history = turns
                .into_iter()
                .flat_map(|(user, assistant)| {
                    [format!("User: {user}"), format!("Assistant: {assistant}")]
                })
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(block(
                ContextBlockKind::RecentMessages,
                &history,
                "journal/conversation",
            ));
        }
        blocks.push(block(
            ContextBlockKind::UserMessage,
            user_text,
            &event.event_id.0,
        ));
        Ok(blocks)
    }

    fn file_block(
        &self,
        kind: ContextBlockKind,
        relative_path: &str,
        fallback: &str,
    ) -> ContextBlock {
        let content = read_text(&self.root_dir.join(relative_path)).unwrap_or(fallback.to_string());
        block(kind, &content, relative_path)
    }

    fn skill_catalog_block(&self) -> ContextBlock {
        let content = skill_catalog(&self.root_dir)
            .unwrap_or_else(|| "chat: basic conversation skill".to_string());
        block(ContextBlockKind::SkillCatalog, &content, "skills/")
    }
    fn tool_catalog_block(
        &self,
        granted_operations: &[String],
        snapshot: &RegistrySnapshot,
    ) -> ContextBlock {
        let content = snapshot.catalog_for_context_grants(granted_operations);
        block(ContextBlockKind::ToolCatalog, &content, "operation/catalog")
    }
}

fn block(kind: ContextBlockKind, content: &str, source_ref: &str) -> ContextBlock {
    ContextBlock {
        kind,
        content: content.trim().to_string(),
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
