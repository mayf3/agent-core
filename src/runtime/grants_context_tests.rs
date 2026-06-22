#[cfg(test)]
mod grants_context_tests {
    use crate::config::KernelConfig;
    use crate::context::ContextAssembler;
    use crate::domain::operation::{
        catalog_for_context_grants, provider_tools_for_grants, ExecutionProfile,
    };
    use crate::domain::*;
    use crate::gateway::Gateway;
    use crate::journal::JournalStore;
    use crate::llm::{LlmClient, LlmInput, OpenAiCompatibleLlm};
    use crate::runtime::Runtime;
    use anyhow::Result;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    // ===== §4: env → profile → principal → ToolCatalog / Provider tools =====
    //
    // The only correct config key is AGENT_CORE_EXTRA_ALLOWED_OPERATIONS.
    // Tests reuse the production `parse_env_list_value` (not a mirror), so
    // the same split/trim/filter logic that `KernelConfig::from_cli` calls
    // is exercised without mutating global environment variables. Downstream
    // with_extra dedups and drops unknown names; Write ops pass the grant
    // check but are hidden by catalog_for_context_grants / provider_tools_for_grants.

    fn tool_set_from_grants(grants: &[String]) -> Vec<String> {
        provider_tools_for_grants(grants)
            .into_iter()
            .filter_map(|t| {
                t.pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
            .collect()
    }

    fn catalog_set_from_grants(grants: &[String]) -> Vec<String> {
        // The catalog text lists "<name> - <desc>" per line after the header.
        catalog_for_context_grants(grants)
            .lines()
            .skip(1)
            .filter_map(|l| l.split(" - ").next().map(str::to_string))
            .collect()
    }

    #[test]
    fn single_time_now_grant_aligns_catalog_and_provider_tools() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value("time.now"))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let tools = tool_set_from_grants(&grants);
        let catalog = catalog_set_from_grants(&grants);
        assert!(tools.contains(&"time.now".to_string()));
        assert!(catalog.contains(&"time.now".to_string()));
        assert_eq!(
            tools, catalog,
            "ToolCatalog set must equal Provider tools set"
        );
    }

    #[test]
    fn multiple_readonly_grants_whitespace_and_duplicates() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value(
                "  time.now ,  system.status ,, time.now  ",
            ))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        // Deduped: time.now appears once.
        let tools = tool_set_from_grants(&grants);
        assert_eq!(
            tools.iter().filter(|t| t == &"time.now").count(),
            1,
            "duplicates deduped"
        );
        // system.status is auto-added by config; both present.
        assert!(tools.contains(&"time.now".to_string()));
        assert!(tools.contains(&"system.status".to_string()));
        assert_eq!(
            tool_set_from_grants(&grants),
            catalog_set_from_grants(&grants)
        );
    }

    #[test]
    fn unknown_operations_do_not_enter_profile_or_tools() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value(
                "shell.exec, time.now, bogus_op",
            ))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let tools = tool_set_from_grants(&grants);
        assert!(!tools.contains(&"shell.exec".to_string()));
        assert!(!tools.contains(&"bogus_op".to_string()));
        assert!(tools.contains(&"time.now".to_string()));
    }

    #[test]
    fn write_operation_granted_but_never_in_tools_or_catalog() {
        // Even if a Write op is in the env, it is granted (lookup passes) but
        // hidden from BOTH Provider tools and the ToolCatalog (ReadOnly-only).
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&crate::config::parse_env_list_value(
                "feishu.send_message, time.now",
            ))
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        assert!(
            grants.contains(&"feishu.send_message".to_string()),
            "write op IS granted (policy is the boundary)"
        );
        let tools = tool_set_from_grants(&grants);
        let catalog = catalog_set_from_grants(&grants);
        assert!(!tools.contains(&"feishu.send_message".to_string()));
        assert!(!catalog.contains(&"feishu.send_message".to_string()));
        assert!(tools.contains(&"time.now".to_string()));
    }

    #[test]
    fn empty_grants_yield_no_tools_and_no_catalog_entries() {
        let grants: Vec<String> = vec![];
        let catalog = catalog_for_context_grants(&grants);
        assert!(
            catalog.contains("No tools are available"),
            "no-grants catalog is explicit, not a full list"
        );
        assert!(provider_tools_for_grants(&grants).is_empty());
    }

    // ===== Context ToolCatalog aligns with grants (§1) =====

    fn empty_event(_session: &Session) -> ValidatedEvent {
        ValidatedEvent {
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
                agent_id: AgentId("main".into()),
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
        }
    }

    fn test_config() -> KernelConfig {
        KernelConfig {
            db_path: PathBuf::from(":memory:"),
            data_dir: PathBuf::from("."),
            agent_id: AgentId("main".into()),
            root_dir: PathBuf::from("/nonexistent-agent-core-root-xyz"),
            kernel_port: 4130,
            connector_execute_url: String::new(),
            ipc_token: "test".into(),
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
            context_recent_messages: 6,
            context_max_block_chars: 4000,
            outbox_dispatcher_enabled: false,
            outbox_dispatcher_poll_interval_ms: 10,
            extra_allowed_operations: vec![],
            require_write_approval: false,
            write_approval_ttl_secs: 0,
        }
    }

    fn build_blocks(grants: &[String]) -> Vec<ContextBlock> {
        let cfg = test_config();
        let journal = JournalStore::in_memory().unwrap();
        let session = Session {
            id: SessionId("s1".into()),
            agent_id: AgentId("main".into()),
            channel: ChannelKind::Cli,
            conversation_key: "local".into(),
            summary: None,
            summarized_until_event_id: None,
            last_active_at: chrono::Utc::now(),
            status: SessionStatus::Active,
            version: 1,
        };
        let event = empty_event(&session);
        ContextAssembler::from_config(&cfg)
            .build(&journal, &session, &event, "hi", grants)
            .unwrap()
    }

    fn catalog_block_text(blocks: &[ContextBlock]) -> String {
        blocks
            .iter()
            .find(|b| matches!(b.kind, ContextBlockKind::ToolCatalog))
            .map(|b| b.content.clone())
            .unwrap_or_default()
    }

    #[test]
    fn context_tool_catalog_omits_ungranted_time_now() {
        // No time.now grant → the ToolCatalog block must not mention it.
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let blocks = build_blocks(&grants);
        let cat = catalog_block_text(&blocks);
        assert!(
            !cat.contains("time.now"),
            "ToolCatalog must omit un-granted time.now: {cat}"
        );
    }

    #[test]
    fn context_tool_catalog_includes_granted_time_now() {
        let grants: Vec<String> = ExecutionProfile::for_channel(ChannelKind::Cli)
            .with_extra(&["time.now".to_string()])
            .grants
            .into_iter()
            .map(|g| g.operation)
            .collect();
        let blocks = build_blocks(&grants);
        let cat = catalog_block_text(&blocks);
        assert!(
            cat.contains("time.now"),
            "granted time.now must be listed: {cat}"
        );
        // Write ops never listed even when granted.
        assert!(!cat.contains("feishu.send_message"));
        assert!(!cat.contains("stdout.send_text"));
    }

    #[test]
    fn context_fallback_contains_no_chat_only_semantics() {
        // When prompt files are absent (root_dir points nowhere), the context
        // uses safe fallback text that must NOT re-introduce Phase-0 semantics.
        let blocks = build_blocks(&[]);
        let all = blocks
            .iter()
            .map(|b| b.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains("chat-only"), "fallback leaked chat-only");
        assert!(!all.contains("Phase 0"), "fallback leaked Phase 0");
        assert!(
            !all.contains("without tools"),
            "fallback leaked without tools"
        );
    }

    #[test]
    fn context_fallback_does_not_leak_paths_or_errors() {
        let blocks = build_blocks(&[]);
        let all = blocks
            .iter()
            .map(|b| b.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !all.contains("nonexistent-agent-core-root"),
            "fallback leaked a file path: {all}"
        );
        assert!(
            !all.contains("No such file") && !all.contains("os error"),
            "fallback leaked an I/O error"
        );
    }

    // ===== IndexedMapping via capture =====

    use crate::domain::JournalEventKind;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    struct _Cap {
        port: u16,
        cap: Arc<Mutex<Vec<Value>>>,
        sd: Arc<AtomicBool>,
        _h: Option<std::thread::JoinHandle<()>>,
    }
    impl _Cap {
        fn start(responses: Vec<Value>) -> Self {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let p = l.local_addr().unwrap().port();
            let c = Arc::new(Mutex::new(Vec::new()));
            let c2 = Arc::clone(&c);
            let sd = Arc::new(AtomicBool::new(false));
            let s2 = Arc::clone(&sd);
            let h = std::thread::spawn(move || {
                for resp in responses {
                    if let Ok((mut s, _)) = l.accept() {
                        if s2.load(Ordering::Relaxed) {
                            break;
                        }
                        let mut buf = [0u8; 4096];
                        let n = s.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            continue;
                        }
                        let hdr = String::from_utf8_lossy(&buf[..n]);
                        let cl = hdr
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                            })
                            .flatten()
                            .unwrap_or(0);
                        let he = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0);
                        let bs = he + 4;
                        if cl > 0 && bs + cl <= n {
                            if let Ok(body) = serde_json::from_slice(&buf[bs..bs + cl]) {
                                c2.lock().unwrap().push(body);
                            }
                        }
                        let b = resp.to_string();
                        let _ = s.write_all(
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                b.len(), b).as_bytes());
                    }
                }
            });
            Self {
                port: p,
                cap: c,
                sd,
                _h: Some(h),
            }
        }
        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/v1", self.port)
        }
        fn reqs(&self) -> Vec<Value> {
            self.cap.lock().unwrap().clone()
        }
    }
    impl Drop for _Cap {
        fn drop(&mut self) {
            self.sd.store(true, Ordering::Relaxed);
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        }
    }

    #[test]
    fn indexed_mapping_encodes_fn_0_in_request() -> Result<()> {
        let srv = _Cap::start(vec![
            // Primary request: the _Cap acts as the LLM endpoint.
            json!({"model":"s","choices":[{"message":{"content":"","tool_calls":[{"id":"x","type":"function","function":{"name":"fn_0","arguments":"{}"}}]}}]}),
            json!({"model":"s","choices":[{"message":{"content":"done"}}]}),
        ]);
        let mut c = super::super::grant_schema_tests::_cfg();
        c.extra_allowed_operations = vec!["time.now".to_string()];
        // Use the _Cap as the PRIMARY endpoint with IndexedMapping.
        let llm = OpenAiCompatibleLlm::new(srv.url(), "t".into(), "p".into(), 5000)
            .with_indexed_primary();
        let j = JournalStore::in_memory()?;
        let g = Gateway::new(c.clone());
        let r = Runtime::new(c, llm);
        let o = r.deliver(
            &j,
            &g,
            g.validate_ingress(&j, g.cli_ingress("time?".to_string())?)?,
        )?;
        assert!(!o.output.trim().is_empty());
        let reqs = srv.reqs();
        assert!(!reqs.is_empty(), "request captured");
        let ns: Vec<&str> = reqs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert!(ns.contains(&"fn_0"), "must encode fn_0, got {ns:?}");
        assert!(
            !ns.iter().any(|n| n.contains("time.now")),
            "no canonical in tools"
        );
        if reqs.len() > 1 {
            let n2: Vec<&str> = reqs[1]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
                .collect();
            assert_eq!(n2, ns, "round-2 tools = round-1");
        }
        let ev = j.events()?;
        let tnp = |k: JournalEventKind| {
            ev.iter()
                .filter(|e| {
                    e.kind == k
                        && e.payload.get("operation").and_then(Value::as_str) == Some("time.now")
                })
                .count()
        };
        assert_eq!(tnp(JournalEventKind::ToolCallIssued), 1);
        assert_eq!(tnp(JournalEventKind::InvocationProposed), 1);
        assert_eq!(tnp(JournalEventKind::InvocationApproved), 1);
        assert_eq!(
            ev.iter()
                .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
                .count(),
            1
        );
        assert_ne!(j.run_status(&o.run_id)?.as_deref(), Some("Running"));
        Ok(())
    }

    #[test]
    fn indexed_mapping_rejects_forged_fn_99() -> Result<()> {
        let srv = _Cap::start(vec![
            json!({"model":"s","choices":[{"message":{"content":"","tool_calls":[{"id":"f","type":"function","function":{"name":"fn_99","arguments":"{}"}}]}}]}),
            json!({"model":"s","choices":[{"message":{"content":"no"}}]}),
        ]);
        let mut c = super::super::grant_schema_tests::_cfg();
        c.extra_allowed_operations = vec!["time.now".to_string()];
        let llm = OpenAiCompatibleLlm::new(srv.url(), "t".into(), "p".into(), 5000)
            .with_indexed_primary();
        let j = JournalStore::in_memory()?;
        let g = Gateway::new(c.clone());
        let r = Runtime::new(c, llm);
        let o = r.deliver(
            &j,
            &g,
            g.validate_ingress(&j, g.cli_ingress("t".to_string())?)?,
        )?;
        let ev = j.events()?;
        let tnp = |k: JournalEventKind| {
            ev.iter()
                .filter(|e| {
                    e.kind == k
                        && e.payload.get("operation").and_then(Value::as_str) == Some("time.now")
                })
                .count()
        };
        assert_eq!(
            tnp(JournalEventKind::InvocationApproved),
            0,
            "forged fn_99 not approved"
        );
        assert_eq!(
            tnp(JournalEventKind::ReceiptReceived),
            0,
            "no time.now exec"
        );
        assert_ne!(j.run_status(&o.run_id)?.as_deref(), Some("Running"));
        Ok(())
    }
} // end mod grants_context_tests
