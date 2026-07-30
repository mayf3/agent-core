//! Domain types for the context-block abstraction used during LLM interaction.
//!
//! A `ContextBlock` is a source item offered to the Model Adapter. The Kernel
//! does not attach selection, truncation, or summarization policy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlock {
    pub kind: ContextBlockKind,
    pub content: String,
    pub source_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContextBlockKind {
    RootSystem,
    RuntimeContract,
    AgentProfile,
    SkillCatalog,
    ToolCatalog,
    ToolResult,
    ActiveSkill,
    RecentMessages,
    UserMessage,
}
