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
        let agent_id = &session.agent_id.0;
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
                &format!("agents/{agent_id}/AGENT.md"),
                "You assist the user by answering messages and, when useful, \
                 calling the tools explicitly provided in the current request. \
                 Prefer an authorized read-only tool over guessing for real-time, \
                 system, or session facts. Do not assume tools that were not \
                 provided or not authorized.",
            ),
            self.workspace_block(agent_id),
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

    /// Per-agent workspace block: lists the files under
    /// `agents/<agent_id>/workspace/` so the model sees exactly this agent's
    /// workspace. A missing or empty directory is reported explicitly rather
    /// than fabricated.
    fn workspace_block(&self, agent_id: &str) -> ContextBlock {
        let dir = self.root_dir.join(format!("agents/{agent_id}/workspace"));
        let content = match std::fs::read_dir(&dir) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .filter_map(Result::ok)
                    .map(|entry| {
                        let mut name = entry.file_name().to_string_lossy().to_string();
                        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            name.push('/');
                        }
                        name
                    })
                    .collect();
                names.sort();
                if names.is_empty() {
                    "workspace is empty".to_string()
                } else {
                    format!("workspace files:\n{}", names.join("\n"))
                }
            }
            Err(_) => "workspace directory not created yet".to_string(),
        };
        block(
            ContextBlockKind::WorkspaceRoot,
            &content,
            &format!("agents/{agent_id}/workspace/"),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use std::path::PathBuf;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agent-core-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &PathBuf, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn config_with_root(root: PathBuf) -> KernelConfig {
        let mut cfg = crate::config::KernelConfig {
            db_path: PathBuf::from(":memory:"),
            data_dir: PathBuf::from("."),
            agent_id: AgentId("main".into()),
            root_dir: PathBuf::from("."),
            kernel_port: 4130,
            connector_execute_url: String::new(),
            ipc_token: "test".into(),
            capability_submit_token: None,
            capability_decision_token: None,
            feishu_allowed_open_ids: vec![],
            feishu_allowed_chat_ids: vec![],
            feishu_require_group_mention: true,
            openai_base_url: String::new(),
            openai_api_key: String::new(),
            model: String::new(),
            fallback_openai_base_url: String::new(),
            fallback_openai_api_key: String::new(),
            fallback_model: String::new(),
            model_timeout_ms: 100,
            outbox_dispatcher_enabled: false,
            outbox_dispatcher_poll_interval_ms: 10,
            extra_allowed_operations: vec![],
            require_write_approval: false,
            write_approval_ttl_secs: 0,
            fallback_tool_name_indexed: false,
            primary_tool_name_indexed: false,
            harness_read_timeout_ms: 10_000,
            harness_artifact_root: std::env::temp_dir().join(format!(
                "ha_root_{}",
                std::process::id()
            )),
            max_tool_rounds: 12,
            feishu_coding_owner_id: None,
            tool_loop_timeout_ms: 300_000,
            context_prepare_hook: crate::hook::HookConfig::default(),
            budget_hook: crate::hook::HookConfig::default(),
        };
        cfg.root_dir = root;
        cfg
    }

    fn session(agent_id: &str, conversation_key: &str) -> Session {
        Session {
            id: SessionId("s1".into()),
            agent_id: AgentId(agent_id.into()),
            channel: ChannelKind::Cli,
            conversation_key: conversation_key.to_string(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: chrono::Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        }
    }

    fn build(root: PathBuf, agent_id: &str) -> Vec<ContextBlock> {
        let cfg = config_with_root(root);
        let assembler = ContextAssembler::from_config(&cfg);
        let journal = JournalStore::in_memory().unwrap();
        let sess = session(agent_id, "local");
        let event = ValidatedEvent {
            event_id: EventId::new(),
            source: EventSource::Cli,
            principal: RunPrincipal {
                principal_id: PrincipalId("cli:local".into()),
                subject: PrincipalSubject::LocalUser,
                source: PrincipalSource::Cli,
                grants: vec![],
                requester_id: Some("cli:local".into()),
            },
            session_target: SessionTarget {
                agent_id: sess.agent_id.clone(),
                channel: ChannelKind::Cli,
                conversation_key: "local".into(),
            },
            payload: RuntimeEventPayload::UserMessage {
                text: "hi".into(),
                message_id: None,
                chat_id: None,
            },
            dedupe_key: format!("dedupe-{}", uuid::Uuid::new_v4()),
            occurred_at: chrono::Utc::now(),
            chat_type: None,
        };
        assembler
            .build(&journal, &sess, &event, "hi", &[], &crate::registry::snapshot::test_snapshot())
            .unwrap()
    }

    fn block_text<'a>(blocks: &'a [ContextBlock], kind: ContextBlockKind) -> &'a str {
        blocks
            .iter()
            .find(|b| b.kind == kind)
            .map(|b| b.content.as_str())
            .unwrap_or_default()
    }

    #[test]
    fn agents_read_isolated_agent_profiles() {
        let root = temp_root();
        write(&root, "agents/main/AGENT.md", "MAIN PROFILE CONTENT");
        write(&root, "agents/worker-a/AGENT.md", "WORKER-A PROFILE CONTENT");
        let main_blocks = build(root.clone(), "main");
        let worker_blocks = build(root.clone(), "worker-a");
        let _ = std::fs::remove_dir_all(&root);
        let main_profile = block_text(&main_blocks, ContextBlockKind::AgentProfile);
        let worker_profile = block_text(&worker_blocks, ContextBlockKind::AgentProfile);
        assert!(main_profile.contains("MAIN PROFILE CONTENT"));
        assert!(!main_profile.contains("WORKER-A PROFILE CONTENT"));
        assert!(worker_profile.contains("WORKER-A PROFILE CONTENT"));
        assert!(!worker_profile.contains("MAIN PROFILE CONTENT"));
    }

    #[test]
    fn agents_read_isolated_workspaces() {
        let root = temp_root();
        write(&root, "agents/main/workspace/main.txt", "main data");
        write(&root, "agents/worker-a/workspace/worker.txt", "worker data");
        let main_blocks = build(root.clone(), "main");
        let worker_blocks = build(root.clone(), "worker-a");
        let _ = std::fs::remove_dir_all(&root);
        let main_ws = block_text(&main_blocks, ContextBlockKind::WorkspaceRoot);
        let worker_ws = block_text(&worker_blocks, ContextBlockKind::WorkspaceRoot);
        assert!(main_ws.contains("main.txt"));
        assert!(!main_ws.contains("worker.txt"));
        assert!(worker_ws.contains("worker.txt"));
        assert!(!worker_ws.contains("main.txt"));
    }
}
