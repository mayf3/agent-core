#[cfg(test)]
mod tool_name_mode_tests {
    use crate::domain::JournalEventKind;
    use crate::gateway::Gateway;
    use crate::journal::JournalStore;
    use crate::llm::{LlmClient, OpenAiCompatibleLlm};
    use crate::runtime::Runtime;
    use anyhow::Result;
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    // Capture stub: reads the full request, captures the JSON body, returns the
    // next queued response. Robust to large bodies.
    struct Capture {
        port: u16,
        reqs: Arc<Mutex<Vec<Value>>>,
    }
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
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let bs = pos + 4;
                            let clen = std::str::from_utf8(&buf[..bs])
                                .unwrap_or("")
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                })
                                .flatten()
                                .unwrap_or(0);
                            if clen == 0 || buf.len() >= bs + clen {
                                break;
                            }
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
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length:{}\r\nConnection:close\r\n\r\n{}",
                                rb.len(), rb
                            )
                            .as_bytes(),
                        );
                    }
                }
            });
            Self { port, reqs }
        }
        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/v1", self.port)
        }
        fn requests(&self) -> Vec<Value> {
            self.reqs.lock().unwrap().clone()
        }
    }

    // Primary stub: returns 429 for every connection; counts hits.
    struct Primary429 {
        port: u16,
        hits: Arc<Mutex<usize>>,
    }
    impl Primary429 {
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
                    let _ = s.write_all(
                        b"HTTP/1.1 429 Too Many Requests\r\nContent-Length:2\r\nConnection:close\r\n\r\n{}",
                    );
                }
            });
            Self { port, hits }
        }
        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/v1", self.port)
        }
        fn hits(&self) -> usize {
            *self.hits.lock().unwrap()
        }
    }

    fn cfg() -> crate::config::KernelConfig {
        super::super::tool_loop_tests::test_config()
    }

    #[test]
    fn indexed_mapping_rejects_forged_fn_99() -> Result<()> {
        let s = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"","tool_calls":[{"id":"f","type":"function","function":{"name":"fn_99","arguments":"{}"}}]}}]}),
            json!({"model":"s","choices":[{"message":{"content":"no"}}]}),
        ]);
        let mut c = cfg();
        c.extra_allowed_operations = vec!["system.status".to_string()];
        let llm =
            OpenAiCompatibleLlm::new(s.url(), "t".into(), "p".into(), 5000).with_indexed_primary();
        let j = JournalStore::in_memory()?;
        let g = Gateway::new(c.clone());
        let r = Runtime::new(c, llm);
        let o = r.deliver(
            &j,
            &g,
            g.validate_ingress(&j, g.cli_ingress("t".to_string())?)?,
        )?;
        let ev = j.events()?;
        let total = |k: JournalEventKind| ev.iter().filter(|e| e.kind == k).count();
        let total_op = |k: JournalEventKind, op: &str| {
            ev.iter()
                .filter(|e| {
                    e.kind == k && e.payload.get("operation").and_then(Value::as_str) == Some(op)
                })
                .count()
        };
        // The forged fn_99 is Malformed → ToolCallIssued + ToolCallRejected.
        assert_eq!(total(JournalEventKind::ToolCallIssued), 1, "Issued=1");
        assert_eq!(total(JournalEventKind::ToolCallRejected), 1, "Rejected=1");
        let rejected = ev
            .iter()
            .find(|e| e.kind == JournalEventKind::ToolCallRejected)
            .unwrap();
        assert_eq!(
            rejected
                .payload
                .get("error_category")
                .and_then(Value::as_str),
            Some("malformed_tool_call"),
            "rejection category must be malformed_tool_call"
        );
        // No system.status Proposed/Approved/Receipt — capability never executes.
        // (stdout.send_text reply Proposed may appear; that is expected and
        // unrelated to the tool call.)
        assert_eq!(
            total_op(JournalEventKind::InvocationProposed, "system.status"),
            0
        );
        assert_eq!(
            total_op(JournalEventKind::InvocationApproved, "system.status"),
            0
        );
        assert_eq!(
            total_op(JournalEventKind::ReceiptReceived, "system.status"),
            0
        );
        // No Approved/Receipt under the system.status tool path (capability never
        // executes). The stdout.send_text reply may itself be Proposed/Approved
        // by Gateway — that is the normal reply path, not the forged tool.
        assert_eq!(
            total_op(JournalEventKind::InvocationApproved, "system.status"),
            0,
            "no system.status Approved"
        );
        assert_eq!(
            total_op(JournalEventKind::ReceiptReceived, "system.status"),
            0,
            "no system.status Receipt"
        );
        // Journal must not contain the raw forged wire name.
        let jt = serde_json::to_string(&ev).unwrap_or_default();
        assert!(
            !jt.contains("fn_99"),
            "forged name must not appear in journal"
        );
        // Exactly one OutboxQueued (the recovery reply), no duplicates/blanks.
        assert_eq!(total(JournalEventKind::OutboxQueued), 1, "OutboxQueued=1");
        // Run is terminal, not Running.
        let st = j.run_status(&o.run_id)?;
        assert_ne!(st.as_deref(), Some("Running"), "Run not stuck Running");
        Ok(())
    }

    #[test]
    fn url_without_deepseek_with_indexed_uses_fn_n() -> Result<()> {
        let fb = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"ok"}}]}),
        ]);
        let snap = crate::registry::snapshot::test_snapshot();
        let provider_tools = snap.provider_tools_for_grants(&["system.status".to_string()]);
        let llm =
            OpenAiCompatibleLlm::new(fb.url(), "t".into(), "p".into(), 5000).with_indexed_primary();
        let _ = llm.complete(crate::llm::LlmInput {
            blocks: vec![],
            user_text: "x".into(),
            granted_operations: vec!["system.status".to_string()],
            provider_tools,
            follow_ups: vec![],
        })?;
        let reqs = fb.requests();
        let ns: Vec<&str> = reqs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert!(
            ns.contains(&"fn_0"),
            "indexed mode applies regardless of URL"
        );
        Ok(())
    }

    #[test]
    fn url_with_deepseek_passthrough_sends_canonical() -> Result<()> {
        let fb = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"ok"}}]}),
        ]);
        let snap = crate::registry::snapshot::test_snapshot();
        let provider_tools = snap.provider_tools_for_grants(&["system.status".to_string()]);
        // Put "deepseek" in the path; mode stays Passthrough (no with_indexed_*).
        let url = fb.url().replace("/v1", "/deepseek/v1");
        let llm = OpenAiCompatibleLlm::new(url, "t".into(), "p".into(), 5000);
        let _ = llm.complete(crate::llm::LlmInput {
            blocks: vec![],
            user_text: "x".into(),
            granted_operations: vec!["system.status".to_string()],
            provider_tools,
            follow_ups: vec![],
        })?;
        let reqs = fb.requests();
        assert!(!reqs.is_empty(), "request captured");
        let ns: Vec<&str> = reqs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert!(
            ns.contains(&"system.status"),
            "passthrough sends canonical even with deepseek URL: {ns:?}"
        );
        assert!(
            !ns.contains(&"fn_0"),
            "fn_N must NOT be used in passthrough"
        );
        Ok(())
    }

    #[test]
    fn primary_429_triggers_indexed_fallback_two_rounds() -> Result<()> {
        let p = Primary429::new();
        let fb = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"","tool_calls":[{"id":"fb1","type":"function","function":{"name":"fn_1","arguments":"{}"}}]}}]}),
            json!({"model":"s","choices":[{"message":{"content":"done"}}]}),
        ]);
        let mut c = cfg();
        c.extra_allowed_operations = vec!["system.status".to_string()];
        c.openai_api_key = "t".into();
        c.model = "p".into();
        c.fallback_tool_name_indexed = true;
        c.fallback_openai_base_url = fb.url();
        c.fallback_openai_api_key = "test".into();
        c.fallback_model = "fs".into();
        // Primary URL is always the 429 stub (build_llm_from_config reads config).
        c.openai_base_url = p.url();
        let llm = crate::server::build_llm_from_config(&c);
        let j = JournalStore::in_memory()?;
        let g = Gateway::new(c.clone());
        let r = Runtime::new(c, llm);
        let o = r.deliver(
            &j,
            &g,
            g.validate_ingress(&j, g.cli_ingress("time?".to_string())?)?,
        )?;
        assert!(!o.output.trim().is_empty());
        let reqs = fb.requests();
        assert_eq!(reqs.len(), 2, "fallback served 2 rounds");
        // Primary is hit exactly once (initial round 429) because follow-up
        // routing is sticky to the fallback endpoint.
        assert_eq!(p.hits(), 1, "primary hit exactly once");
        let ns: Vec<&str> = reqs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert!(ns.contains(&"fn_0"), "fallback tools use fn_N");
        assert!(!ns.iter().any(|n| n.contains("system.status")));
        // Round-2 tools == round-1 (each round rebuilds its own map).
        let n2: Vec<&str> = reqs[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert_eq!(n2, ns, "round-2 tools == round-1");
        let ev = j.events()?;
        let tnp = |k: JournalEventKind| {
            ev.iter()
                .filter(|e| {
                    e.kind == k
                        && e.payload.get("operation").and_then(Value::as_str)
                            == Some("system.status")
                })
                .count()
        };
        assert_eq!(tnp(JournalEventKind::ToolCallIssued), 1);
        assert_eq!(tnp(JournalEventKind::InvocationProposed), 1);
        assert_eq!(tnp(JournalEventKind::InvocationApproved), 1);
        // Receipt correlation: shares the invocation_id (correlation_id) with
        // Proposed/Approved — not just a count check.
        let approved = ev
            .iter()
            .find(|e| {
                e.kind == JournalEventKind::InvocationApproved
                    && e.payload.get("operation").and_then(Value::as_str) == Some("system.status")
            })
            .expect("approved system.status");
        let corr = approved.correlation_id.as_ref().expect("correlation_id");
        assert_eq!(
            approved.run_id.as_ref(),
            Some(&o.run_id),
            "Approved run_id matches"
        );
        let receipt = ev
            .iter()
            .find(|e| e.kind == JournalEventKind::ReceiptReceived)
            .expect("receipt");
        assert_eq!(
            receipt.correlation_id.as_ref(),
            Some(corr),
            "Receipt shares invocation correlation_id"
        );
        assert_eq!(receipt.run_id.as_ref(), Some(&o.run_id));
        assert_eq!(
            receipt.payload.get("status").and_then(Value::as_str),
            Some("Succeeded")
        );
        // Round-2 structured follow-up: the tool result is sent as a
        // role:tool message with tool_call_id matching the assistant tool_calls.
        let round2_messages = reqs[1]["messages"].as_array().unwrap();
        let tool_msg = round2_messages
            .iter()
            .find(|m| m["role"].as_str() == Some("tool"))
            .expect("role:tool message in round 2");
        let tool_content = tool_msg["content"].as_str().unwrap_or("");
        assert!(
            tool_content.contains("system.status") || tool_content.contains("succeeded"),
            "role:tool content references the tool result: {tool_content}"
        );
        // The assistant tool_call_id and tool.tool_call_id must match.
        let assistant_msg = round2_messages
            .iter()
            .find(|m| m["role"].as_str() == Some("assistant"))
            .expect("role:assistant message in round 2");
        let call_id = assistant_msg["tool_calls"][0]["id"].as_str().unwrap_or("");
        assert!(
            !call_id.is_empty(),
            "assistant tool_calls[0].id is the raw provider id"
        );
        assert_eq!(
            tool_msg["tool_call_id"].as_str(),
            Some(call_id),
            "tool.tool_call_id matches assistant tool_calls[0].id"
        );
        assert_eq!(
            tnp(JournalEventKind::InvocationApproved),
            1,
            "no 2nd capability exec"
        );
        assert_eq!(
            ev.iter()
                .filter(|e| e.kind == JournalEventKind::OutboxQueued)
                .count(),
            1
        );
        let st = j.run_status(&o.run_id)?;
        assert!(
            st.as_deref() == Some("WaitingDispatch") || st.as_deref() == Some("Completed"),
            "Run terminal, got {st:?}"
        );
        let jt = serde_json::to_string(&ev).unwrap_or_default();
        assert!(
            !jt.contains("fn_0") && !jt.contains("fn_1"),
            "fn_N must not appear in journal"
        );
        Ok(())
    }

    //
    // Exercises KernelConfig → build_llm_from_config → ModelEndpoint wiring,
    // the SAME path delivery.rs uses. No HTTP needed: the indexed flags are
    // inspected directly. Primary and fallback are independent.
}

#[test]
fn env_bool_accepts_common_truthy_values() {
    use crate::config::parse_env_bool_value;
    for v in ["true", "TRUE", "True", "1", "yes", "YES", "on", "on "] {
        assert_eq!(parse_env_bool_value(v), Ok(true), "truthy: {v:?}");
    }
    for v in ["false", "FALSE", "0", "no", "off", "OFF"] {
        assert_eq!(parse_env_bool_value(v), Ok(false), "falsy: {v:?}");
    }
}

#[test]
fn env_bool_unparsable_returns_error() {
    use crate::config::parse_env_bool_value;
    for v in ["", "maybe", "2", "y", "n", "yes/no", "tru"] {
        let r = parse_env_bool_value(v);
        assert!(r.is_err(), "invalid value must produce Err: {v:?}");
        let err = r.unwrap_err();
        assert_eq!(err, "invalid_boolean_config");
        if v.len() > 2 {
            assert!(!err.contains(v), "error must not contain raw value: {v:?}");
        }
    }
}
