//! Context Harness V0 — external ContextPrepareV0 provider for the
//! standalone agent-runtime.
//!
//! This is a Bootstrap wiring piece, NOT a product special case. It reads
//! the resolved Agent workspace's `AGENTS.md` (an opaque external working
//! instruction owned by the Agent, not by the Kernel) and injects it into
//! the ContextPrepareV0 candidate as an additional `SupplementalSystem`
//! block (provider-owned system authority).
//!
//! Invariants (tested):
//! - user_text is preserved verbatim;
//! - provider tools are preserved verbatim;
//! - all original candidate blocks are preserved;
//! - only an Agent instruction block is ADDED (AGENTS.md content);
//! - a missing AGENTS.md degrades to the unmodified candidate (fail-open);
//! - the provider proof uses the same HMAC scheme as the Kernel.
//!
//! The workspace root is resolved OUTSIDE this binary (host/deployment
//! config): `CONTEXT_HARNESS_WORKSPACE_ROOT` points at the already-resolved
//! Agent workspace (e.g. `~/.agent-core/workspace/main`). The Kernel ABI
//! carries no workspace/AGENTS.md semantics.

use agent_core_kernel::domain::{ContextBlock, ContextBlockKind};
use agent_core_kernel::llm::LlmInput;
use agent_core_kernel::hook::{
    compute_provider_proof, ContextHookRequest, ContextHookResponse, HookKind,
    HookResponseEnvelope, OpaqueArtifactRef,
};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

const DEFAULT_AGENTS_MD: &str = "AGENTS.md";

fn main() -> Result<()> {
    let port: u16 = std::env::var("CONTEXT_HARNESS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7401);
    let workspace_root = std::env::var("CONTEXT_HARNESS_WORKSPACE_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|home| PathBuf::from(home).join(".agent-core/workspace/main"))
                .unwrap_or_else(|_| PathBuf::from(".agent-core/workspace/main"))
        });
    let secret = std::env::var("CONTEXT_HARNESS_SHARED_SECRET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("CONTEXT_HARNESS_SHARED_SECRET is required"))?;
    let provider_id = std::env::var("CONTEXT_HARNESS_PROVIDER_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "agent-context-harness-v0".to_string());

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("bind {port}"))?;
    eprintln!(
        "context-harness listening on 127.0.0.1:{port} workspace={}",
        workspace_root.display()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle(
                    stream,
                    &workspace_root,
                    &secret,
                    &provider_id,
                ) {
                    eprintln!("context-harness handler error: {e}");
                }
            }
            Err(e) => eprintln!("context-harness accept error: {e}"),
        }
    }
    Ok(())
}

/// ADD the Agent workspace's AGENTS.md as one extra SupplementalSystem
/// block (provider-owned system authority — NOT the agent profile).
/// Everything else in the candidate is preserved verbatim. A missing or
/// empty AGENTS.md degrades to the unmodified candidate (fail-open).
fn inject_agents_md(input: &mut LlmInput, workspace_root: &PathBuf) {
    let agents_md = workspace_root.join(DEFAULT_AGENTS_MD);
    if let Ok(content) = std::fs::read_to_string(&agents_md) {
        let content = content.trim();
        if !content.is_empty() {
            input.blocks.push(ContextBlock {
                kind: ContextBlockKind::SupplementalSystem,
                content: content.to_string(),
                source_ref: Some(agents_md.to_string_lossy().to_string()),
            });
        }
    }
}

fn handle(
    mut stream: TcpStream,
    workspace_root: &PathBuf,
    secret: &str,
    provider_id: &str,
) -> Result<()> {
    let request_bytes = read_http_request(&mut stream)?;
    let request_text = String::from_utf8(request_bytes)?;
    let (headers, body) = request_text
        .split_once("\r\n\r\n")
        .context("http body")?;
    let authorized = headers.lines().any(|line| {
        line.eq_ignore_ascii_case(&format!("authorization: Bearer {secret}"))
    });
    if !authorized {
        write_http(&mut stream, 401, &[], "{}")?;
        return Ok(());
    }
    let envelope: Value = serde_json::from_str(body)?;
    let request: ContextHookRequest =
        serde_json::from_value(envelope.get("payload").cloned().context("payload")?)?;
    if request.candidate.artifact.media_type
        != agent_core_kernel::llm::LLM_INPUT_ARTIFACT_MEDIA_TYPE
    {
        bail!("unexpected candidate media type");
    }

    let mut input: LlmInput = serde_json::from_slice(&request.candidate.artifact.decode_verified()?)?;
    inject_agents_md(&mut input, workspace_root);

    let artifact = OpaqueArtifactRef::new(
        request.candidate.artifact.media_type.clone(),
        &serde_json::to_vec(&input)?,
    );
    let response = ContextHookResponse {
        run_id: request.candidate.run_id.clone(),
        session_id: request.candidate.session_id.clone(),
        scope_digest: request.candidate.scope_digest.clone(),
        candidate_digest: request.candidate.artifact.digest.clone(),
        immutable_refs: request.candidate.immutable_refs.clone(),
        immutable_refs_digest: request.candidate.immutable_refs_digest.clone(),
        artifacts: vec![artifact],
    };
    let proof = compute_provider_proof(
        secret,
        &response.authentication_message(provider_id, &request.request_id),
    )?;
    let response_envelope = HookResponseEnvelope {
        request_id: request.request_id,
        hook: HookKind::ContextPrepareV0,
        timestamp: Utc::now(),
        payload: serde_json::to_value(response)?,
    };
    write_http(
        &mut stream,
        200,
        &[("X-Agent-Core-Provider-Proof", proof.as_str())],
        &serde_json::to_string(&response_envelope)?,
    )
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            // header end seen; read the remaining content-length body
            let text = String::from_utf8_lossy(&buffer);
            if let Some(cl) = text
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            {
                let len: usize = cl
                    .split(':')
                    .nth(1)
                    .map(str::trim)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let header_end = text.find("\r\n\r\n").unwrap_or(0) + 4;
                while buffer.len() < header_end + len {
                    let m = stream.read(&mut chunk)?;
                    if m == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..m]);
                }
            }
            break;
        }
        if buffer.len() > 2 * 1024 * 1024 {
            bail!("request too large");
        }
    }
    Ok(buffer)
}

fn write_http(stream: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &str) -> Result<()> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let mut resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("Connection: close\r\n\r\n");
    resp.push_str(body);
    stream.write_all(resp.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::llm::{LlmClient, LlmInput};

    fn sample_input() -> LlmInput {
        LlmInput {
            blocks: vec![ContextBlock {
                kind: ContextBlockKind::RootSystem,
                content: "system".into(),
                source_ref: None,
            }],
            user_text: "请执行任务".into(),
            granted_operations: vec!["external.coding_workspace_exec".into()],
            provider_tools: vec![serde_json::json!({"function": {"name": "t1"}})],
            follow_ups: vec![],
            timeout_override_ms: None,
        }
    }

    #[test]
    fn harness_preserves_user_text_tools_and_original_blocks() {
        let dir = std::env::temp_dir().join(format!("ctx-harness-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("AGENTS.md"),
            "# main agent\n- 搜索优先\n- 修改后检查 git diff\n",
        )
        .unwrap();

        let original = sample_input();
        let mut transformed = original.clone();
        inject_agents_md(&mut transformed, &dir);

        // user_text verbatim
        assert_eq!(transformed.user_text, original.user_text);
        // tools verbatim
        assert_eq!(transformed.provider_tools, original.provider_tools);
        assert_eq!(transformed.granted_operations, original.granted_operations);
        // original blocks preserved (first block unchanged, count +1)
        assert_eq!(transformed.blocks.len(), original.blocks.len() + 1);
        assert_eq!(transformed.blocks[0].kind, original.blocks[0].kind);
        assert_eq!(transformed.blocks[0].content, original.blocks[0].content);
        // AGENTS.md added as SupplementalSystem (provider-owned system
        // authority — NOT the agent profile).
        let added = transformed.blocks.last().unwrap();
        assert_eq!(added.kind, ContextBlockKind::SupplementalSystem);
        assert!(added.content.contains("搜索优先"));
        assert_eq!(transformed.follow_ups.len(), original.follow_ups.len());
    }

    #[test]
    fn missing_agents_md_degrades_to_unmodified_candidate() {
        let dir = std::env::temp_dir().join(format!("ctx-harness-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap(); // no AGENTS.md

        let original = sample_input();
        let mut transformed = original.clone();
        inject_agents_md(&mut transformed, &dir);
        assert_eq!(transformed.blocks.len(), original.blocks.len(), "no injection without AGENTS.md");
        assert_eq!(transformed.user_text, original.user_text);
    }

    #[test]
    fn llm_input_json_roundtrips_like_the_wire_format() {
        // The ContextPrepareV0 artifact is the serialized LlmInput; the
        // harness decodes and re-encodes it. Verify field-level stability.
        let input = sample_input();
        let bytes = serde_json::to_vec(&input).unwrap();
        let decoded: LlmInput = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.user_text, input.user_text);
        assert_eq!(decoded.provider_tools, input.provider_tools);
        assert_eq!(decoded.blocks.len(), input.blocks.len());
        assert_eq!(decoded.granted_operations, input.granted_operations);
    }

    /// A minimal Kernel-side LlmClient: stage_candidate / materialize_context
    /// are the REAL kernel implementations (trait defaults); complete is
    /// never reached in this chain.
    struct ChainLlm;
    impl agent_core_kernel::llm::LlmClient for ChainLlm {
        fn complete(
            &self,
            _input: LlmInput,
        ) -> anyhow::Result<agent_core_kernel::llm::LlmOutput> {
            unreachable!("chain test never completes a model call")
        }
    }

    /// THE REAL CHAIN: base LlmInput → Context Harness injects the real
    /// AGENTS.md (read from disk) → artifact on the v2 wire → Kernel
    /// materialize → SUCCESS. Proves AGENTS.md as SupplementalSystem never
    /// trips `model_adapter_immutable_input_changed`.
    #[test]
    fn agents_md_supplemental_system_materializes_through_kernel() {
        let dir = std::env::temp_dir().join(format!("ctx-harness-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("AGENTS.md"),
            "# main agent\n- 搜索优先\n- 修改后检查 git diff\n",
        )
        .unwrap();

        // Base candidate shaped like the Kernel's ContextAssembler output:
        // RootSystem + RuntimeContract + catalogs + user message.
        let base = LlmInput {
            blocks: vec![
                ContextBlock {
                    kind: ContextBlockKind::RootSystem,
                    content: "root-system".into(),
                    source_ref: Some("system/root.md".into()),
                },
                ContextBlock {
                    kind: ContextBlockKind::RuntimeContract,
                    content: "runtime-contract".into(),
                    source_ref: Some("system/runtime.md".into()),
                },
                ContextBlock {
                    kind: ContextBlockKind::ToolCatalog,
                    content: "tool-catalog".into(),
                    source_ref: Some("operation/catalog".into()),
                },
                ContextBlock {
                    kind: ContextBlockKind::UserMessage,
                    content: "user-msg".into(),
                    source_ref: None,
                },
            ],
            user_text: "请执行任务".into(),
            granted_operations: vec!["external.coding_workspace_exec".into()],
            provider_tools: vec![serde_json::json!({
                "type": "function",
                "function": {"name": "t1", "parameters": {"type": "object"}}
            })],
            follow_ups: vec![],
            timeout_override_ms: None,
        };

        let llm = ChainLlm;
        // 1) Kernel stages the base candidate (records the trusted required
        //    instances as immutable refs).
        let candidate = llm.stage_candidate(base.clone()).unwrap();
        // 2) The Context Harness injects the REAL AGENTS.md content.
        let mut injected = base.clone();
        inject_agents_md(&mut injected, &dir);
        // 3) The harness returns the artifact on the v2 wire.
        let artifact = OpaqueArtifactRef::new(
            agent_core_kernel::llm::LLM_INPUT_ARTIFACT_MEDIA_TYPE,
            &serde_json::to_vec(&injected).unwrap(),
        );
        // 4) Kernel materialize — must SUCCEED (no
        //    model_adapter_immutable_input_changed, no OverBudget).
        let materialized = match llm.materialize_context(&candidate, &[artifact]) {
            Ok(m) => m,
            Err(e) => panic!(
                "materialize must succeed, got: {e} (model_adapter_immutable_input_changed would mean the kind is wrong)"
            ),
        };
        let agent_core_kernel::llm::ModelMaterialization::Ready { input, .. } = materialized
        else {
            panic!("expected Ready materialization");
        };

        // AGENTS.md sits in system authority as SupplementalSystem.
        let added = input.blocks.last().unwrap();
        assert_eq!(added.kind, ContextBlockKind::SupplementalSystem);
        assert!(added.content.contains("搜索优先"));
        assert!(added.content.contains("git diff"));

        // Original trusted content is untouched.
        assert_eq!(input.blocks[0].kind, ContextBlockKind::RootSystem);
        assert_eq!(input.blocks[0].content, "root-system");
        assert_eq!(input.blocks[1].kind, ContextBlockKind::RuntimeContract);
        assert_eq!(input.blocks[1].content, "runtime-contract");
        assert_eq!(input.blocks[2].kind, ContextBlockKind::ToolCatalog);
        assert_eq!(input.blocks[2].content, "tool-catalog");
        assert_eq!(input.user_text, "请执行任务");
        assert_eq!(input.granted_operations, base.granted_operations);
        assert_eq!(input.provider_tools, base.provider_tools);

        // The artifact rides the v2 schema.
        assert!(
            agent_core_kernel::llm::LLM_INPUT_ARTIFACT_MEDIA_TYPE.contains("version=2"),
            "artifact must use the v2 media type"
        );
    }
}
