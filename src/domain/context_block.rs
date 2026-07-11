//! Domain types for the context-block abstraction used during LLM interaction.
//!
//! A `ContextBlock` is a single section of the LLM context window, tagged
//! with a `ContextBlockKind` that controls placement, and a `Compressibility`
//! policy that governs how the runtime can shrink the block when the context
//! window is full.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlock {
    pub kind: ContextBlockKind,
    pub content: String,
    pub compressibility: Compressibility,
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
    /// Context fragment injected by external hooks (context.prepare.v0).
    /// Placed before UserMessage — never enters the immutable system prompt.
    HookFragment,
    /// HCR instructions for external harness creation.
    HarnessChangeRequest,
    UserMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Compressibility {
    Never,
    DropWhole,
    Summarizable,
    Truncate,
}
