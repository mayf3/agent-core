#[cfg(test)]
mod transcript_isolation_tests {
    use crate::gateway::Gateway;
    use crate::journal::JournalStore;
    use crate::llm::{LlmClient, LlmInput, OpenAiCompatibleLlm};
    use crate::runtime::Runtime;
    use anyhow::Result;
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn cfg() -> crate::config::KernelConfig {
        super::super::grant_schema_tests::_cfg()
    }

    /// Capture stub: serves queued responses, captures request bodies.
    struct Capture { port: u16, reqs: Arc<Mutex<Vec<Value>>> }
    impl Capture {
        fn new(responses: Vec<Value>) -> Self {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let reqs = Arc::new(Mutex::new(Vec::new()));
            let r2 = Arc::clone(&reqs);
            thread::spawn(move || {
                let mut it = responses.into_iter();
                while let Ok((mut s, _)) = l.accept() {
                    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(2000)));
                    let mut buf = Vec::with_capacity(8192);
                    let mut tmp = [0u8; 4096];
                    loop {
                        let n = s.read(&mut tmp).unwrap_or(0);
                        if n == 0 { break; }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let bs = pos + 4;
                            let clen = std::str::from_utf8(&buf[..bs]).unwrap_or("")
                                .lines().find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())
                                }).flatten().unwrap_or(0);
                            if clen == 0 || buf.len() >= bs + clen { break; }
                        }
                    }
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        if let Ok(v) = serde_json::from_slice::<Value>(&buf[pos + 4..]) {
                            r2.lock().unwrap().push(v);
                        }
                    }
                    if let Some(resp) = it.next() {
                        let rb = resp.to_string();
                        let _ = s.write_all(
                            format!("HTTP/1.1 200 OK\r\nContent-Length:{}\r\nConnection:close\r\n\r\n{}", rb.len(), rb).as_bytes(),
                        );
                    }
                }
            });
            Self { port, reqs }
        }
        fn url(&self) -> String { format!("http://127.0.0.1:{}/v1", self.port) }
        fn requests(&self) -> Vec<Value> { self.reqs.lock().unwrap().clone() }
    }

    /// 429 stub: returns 429 for every connection.
    struct S429 { port: u16, hits: Arc<Mutex<usize>> }
    impl S429 {
        fn new() -> Self {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let hits = Arc::new(Mutex::new(0usize));
            let h2 = Arc::clone(&hits);
            thread::spawn(move || {
                while let Ok((mut s, _)) = l.accept() {
                    *h2.lock().unwrap() += 1;
                    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(1000)));
                    let mut b = [0u8; 1024];
                    let _ = s.read(&mut b);
                    let _ = s.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length:2\r\nConnection:close\r\n\r\n{}");
                }
            });
            Self { port, hits }
        }
        fn url(&self) -> String { format!("http://127.0.0.1:{}/v1", self.port) }
        fn hits(&self) -> usize { *self.hits.lock().unwrap() }
    }

    fn time_tool_call(raw_id: &str) -> Value {
        json!({"model":"s","choices":[{"message":{"content":"","tool_calls":[{"id":raw_id,"type":"function","function":{"name":"fn_0","arguments":"{}"}}]}}]})
    }

    // ===== §2: first-round request has NO assistant/tool messages =====

    #[test]
    fn first_round_no_followup_has_no_transcript_messages() -> Result<()> {
        let srv = Capture::new(vec![json!({"model":"s","choices":[{"message":{"content":"hello"}}]})]);
        let llm = OpenAiCompatibleLlm::new(srv.url(), "t".into(), "p".into(), 5000).with_indexed_primary();
        let _ = llm.complete(LlmInput {
            blocks: vec![],
            user_text: "hi".into(),
            granted_operations: vec!["time.now".into()],
            follow_up: None,
        })?;
        let reqs = srv.requests();
        assert!(!reqs.is_empty(), "request captured");
        let msgs = reqs[0]["messages"].as_array().unwrap();
        let roles: Vec<&str> = msgs.iter().filter_map(|m| m["role"].as_str()).collect();
        assert_eq!(roles, vec!["system", "user"], "first round must have only system+user");
        assert!(!msgs.iter().any(|m| m["role"].as_str() == Some("assistant")), "no assistant message");
        assert!(!msgs.iter().any(|m| m["role"].as_str() == Some("tool")), "no tool message");
        Ok(())
    }

    // ===== §4: primary stickiness =====

    #[test]
    fn primary_tool_call_followup_stays_on_primary() -> Result<()> {
        let mut c = cfg();
        c.extra_allowed_operations = vec!["time.now".into()];
        c.openai_base_url = String::new(); // primary not configured → skip primary
        c.openai_api_key = String::new();
        // Use primary as the main, fallback as the other endpoint.
        // We set primary = tool-call server, fallback = text server.
        let primary_srv = Capture::new(vec![
            time_tool_call("call_primary_1"),
            json!({"model":"s","choices":[{"message":{"content":"done"}}]}),
        ]);
        let fallback_srv = S429::new();
        c.openai_base_url = primary_srv.url();
        c.openai_api_key = "k".into();
        c.model = "p".into();
        c.fallback_openai_base_url = fallback_srv.url();
        c.fallback_openai_api_key = "fk".into();
        c.fallback_model = "fm".into();
        c.primary_tool_name_indexed = true;
        let llm = crate::server::build_llm_from_config(&c);
        let j = JournalStore::in_memory()?;
        let g = Gateway::new(c.clone());
        let r = Runtime::new(c, llm);
        let o = r.deliver(&j, &g, g.validate_ingress(&j, g.cli_ingress("time?".into())?)?)?;
        assert!(!o.output.trim().is_empty());
        // Primary served both rounds (tool call + follow-up).
        assert_eq!(primary_srv.requests().len(), 2, "primary served 2 rounds");
        assert_eq!(fallback_srv.hits(), 0, "fallback never hit");
        // Round 2 has structured transcript.
        let reqs = primary_srv.requests();
        let msgs = reqs[1]["messages"].as_array().unwrap();
        assert!(msgs.iter().any(|m| m["role"].as_str() == Some("tool")), "round 2 has role:tool");
        // Journal has no raw provider ID or wire name.
        let ev = j.events()?;
        let jt = serde_json::to_string(&ev).unwrap_or_default();
        assert!(!jt.contains("call_primary_1"), "no raw provider ID in journal");
        assert!(!jt.contains("fn_0"), "no wire name in journal");
        Ok(())
    }

    // ===== §3: concurrent isolation — same client, interleaved calls =====

    #[test]
    fn two_runs_isolate_transcript_via_explicit_follow_up() -> Result<()> {
        // Test that each Run's follow_up is independently determined by its own
        // LlmInput, not by shared client state. We call complete() twice with
        // different follow_ups on the SAME client — the second must not see
        // the first's data.
        let srv = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"a_reply"}}]}),
            json!({"model":"s","choices":[{"message":{"content":"b_reply"}}]}),
        ]);
        let llm = Arc::new(
            OpenAiCompatibleLlm::new(srv.url(), "t".into(), "p".into(), 5000).with_indexed_primary()
        );
        // Run A: follow_up with provider id call_A, wire fn_0, result result_A.
        let llm_a = Arc::clone(&llm);
        let handle_a = thread::spawn(move || {
            llm_a.complete(LlmInput {
                blocks: vec![],
                user_text: "a".into(),
                granted_operations: vec![],
                follow_up: Some(crate::llm::LlmFollowUp {
                    provider_turn: crate::llm::ProviderToolTurn {
                        endpoint: crate::llm::EndpointChoice::Primary,
                        provider_tool_call_id: "call_A".into(),
                        wire_name: "fn_0".into(),
                        canonical_operation: "time.now".into(),
                        arguments_json: "{}".into(),
                    },
                    result_content: "status: succeeded\noutput: result_A".into(),
                }),
            })
        });
        // Run B: follow_up with provider id call_B, wire fn_1, result result_B.
        let llm_b = Arc::clone(&llm);
        let handle_b = thread::spawn(move || {
            llm_b.complete(LlmInput {
                blocks: vec![],
                user_text: "b".into(),
                granted_operations: vec![],
                follow_up: Some(crate::llm::LlmFollowUp {
                    provider_turn: crate::llm::ProviderToolTurn {
                        endpoint: crate::llm::EndpointChoice::Primary,
                        provider_tool_call_id: "call_B".into(),
                        wire_name: "fn_1".into(),
                        canonical_operation: "session.recall_recent".into(),
                        arguments_json: "{}".into(),
                    },
                    result_content: "status: succeeded\noutput: result_B".into(),
                }),
            })
        });
        handle_a.join().unwrap()?;
        handle_b.join().unwrap()?;
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2, "two requests captured");
        // Find which request is A and which is B by their follow_up content.
        let a_req = reqs.iter().find(|r| {
            r["messages"].as_array().map(|m| {
                m.iter().any(|msg| {
                    msg["role"].as_str() == Some("tool")
                        && msg["content"].as_str().unwrap_or("").contains("result_A")
                })
            }).unwrap_or(false)
        }).expect("A's request found");
        let b_req = reqs.iter().find(|r| {
            r["messages"].as_array().map(|m| {
                m.iter().any(|msg| {
                    msg["role"].as_str() == Some("tool")
                        && msg["content"].as_str().unwrap_or("").contains("result_B")
                })
            }).unwrap_or(false)
        }).expect("B's request found");
        // A's assistant tool_call has call_A / fn_0.
        let a_assistant = a_req["messages"].as_array().unwrap().iter()
            .find(|m| m["role"].as_str() == Some("assistant")).unwrap();
        assert_eq!(a_assistant["tool_calls"][0]["id"].as_str(), Some("call_A"));
        assert_eq!(a_assistant["tool_calls"][0]["function"]["name"].as_str(), Some("fn_0"));
        let a_tool = a_req["messages"].as_array().unwrap().iter()
            .find(|m| m["role"].as_str() == Some("tool")).unwrap();
        assert_eq!(a_tool["tool_call_id"].as_str(), Some("call_A"));
        // B's assistant tool_call has call_B / fn_1.
        let b_assistant = b_req["messages"].as_array().unwrap().iter()
            .find(|m| m["role"].as_str() == Some("assistant")).unwrap();
        assert_eq!(b_assistant["tool_calls"][0]["id"].as_str(), Some("call_B"));
        assert_eq!(b_assistant["tool_calls"][0]["function"]["name"].as_str(), Some("fn_1"));
        // No cross-contamination: A doesn't have call_B, B doesn't have call_A.
        let a_json = serde_json::to_string(a_req).unwrap();
        assert!(!a_json.contains("call_B"), "A leaked B's id");
        let b_json = serde_json::to_string(b_req).unwrap();
        assert!(!b_json.contains("call_A"), "B leaked A's id");
        Ok(())
    }

    #[test]
    fn same_raw_id_different_runs_still_isolated() -> Result<()> {
        // Both runs use the SAME provider id text "call_same", but different
        // wire names and results. Each follow_up must carry only its own data.
        let srv = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"x"}}]}),
            json!({"model":"s","choices":[{"message":{"content":"y"}}]}),
        ]);
        let llm = Arc::new(
            OpenAiCompatibleLlm::new(srv.url(), "t".into(), "p".into(), 5000).with_indexed_primary()
        );
        let l1 = Arc::clone(&llm);
        let h1 = thread::spawn(move || {
            l1.complete(LlmInput {
                blocks: vec![], user_text: "a".into(), granted_operations: vec![],
                follow_up: Some(crate::llm::LlmFollowUp {
                    provider_turn: crate::llm::ProviderToolTurn {
                        endpoint: crate::llm::EndpointChoice::Primary,
                        provider_tool_call_id: "call_same".into(),
                        wire_name: "fn_0".into(),
                        canonical_operation: "time.now".into(),
                        arguments_json: "{}".into(),
                    },
                    result_content: "result_A".into(),
                }),
            })
        });
        let l2 = Arc::clone(&llm);
        let h2 = thread::spawn(move || {
            l2.complete(LlmInput {
                blocks: vec![], user_text: "b".into(), granted_operations: vec![],
                follow_up: Some(crate::llm::LlmFollowUp {
                    provider_turn: crate::llm::ProviderToolTurn {
                        endpoint: crate::llm::EndpointChoice::Primary,
                        provider_tool_call_id: "call_same".into(),
                        wire_name: "fn_1".into(),
                        canonical_operation: "session.recall_recent".into(),
                        arguments_json: "{}".into(),
                    },
                    result_content: "result_B".into(),
                }),
            })
        });
        h1.join().unwrap()?;
        h2.join().unwrap()?;
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        // Each request has exactly one role:tool with its own result.
        for req in &reqs {
            let tools: Vec<_> = req["messages"].as_array().unwrap().iter()
                .filter(|m| m["role"].as_str() == Some("tool")).collect();
            assert_eq!(tools.len(), 1, "exactly one role:tool per request");
        }
        let a_req = reqs.iter().find(|r| serde_json::to_string(r).unwrap().contains("result_A")).unwrap();
        let b_req = reqs.iter().find(|r| serde_json::to_string(r).unwrap().contains("result_B")).unwrap();
        assert!(!serde_json::to_string(a_req).unwrap().contains("result_B"), "A leaked B's result");
        assert!(!serde_json::to_string(b_req).unwrap().contains("result_A"), "B leaked A's result");
        Ok(())
    }
}
