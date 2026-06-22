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
        super::super::grant_schema_tests::_cfg()
    }

    // ===== forged name rejection (IndexedMapping + unknown fn_99) =====
    #[test]
    fn indexed_mapping_rejects_forged_fn_99() -> Result<()> {
        let s = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"","tool_calls":[{"id":"f","type":"function","function":{"name":"fn_99","arguments":"{}"}}]}}]}),
            json!({"model":"s","choices":[{"message":{"content":"no"}}]}),
        ]);
        let mut c = cfg();
        c.extra_allowed_operations = vec!["time.now".to_string()];
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
        // No time.now Proposed/Approved/Receipt — capability never executes.
        // (stdout.send_text reply Proposed may appear; that is expected and
        // unrelated to the tool call.)
        assert_eq!(
            total_op(JournalEventKind::InvocationProposed, "time.now"),
            0
        );
        assert_eq!(
            total_op(JournalEventKind::InvocationApproved, "time.now"),
            0
        );
        assert_eq!(total_op(JournalEventKind::ReceiptReceived, "time.now"), 0);
        // No Approved/Receipt under the time.now tool path (capability never
        // executes). The stdout.send_text reply may itself be Proposed/Approved
        // by Gateway — that is the normal reply path, not the forged tool.
        assert_eq!(
            total_op(JournalEventKind::InvocationApproved, "time.now"),
            0,
            "no time.now Approved"
        );
        assert_eq!(
            total_op(JournalEventKind::ReceiptReceived, "time.now"),
            0,
            "no time.now Receipt"
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

    // ===== URL without "deepseek" + IndexedMapping → fn_N used =====
    #[test]
    fn url_without_deepseek_with_indexed_uses_fn_n() -> Result<()> {
        let fb = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"ok"}}]}),
        ]);
        let llm =
            OpenAiCompatibleLlm::new(fb.url(), "t".into(), "p".into(), 5000).with_indexed_primary();
        let _ = llm.complete(crate::llm::LlmInput {
            blocks: vec![],
            user_text: "x".into(),
            granted_operations: vec!["time.now".to_string()],
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

    // ===== URL with "deepseek" + Passthrough → canonical operation sent =====
    #[test]
    fn url_with_deepseek_passthrough_sends_canonical() -> Result<()> {
        let fb = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"ok"}}]}),
        ]);
        // Put "deepseek" in the path; mode stays Passthrough (no with_indexed_*).
        let url = fb.url().replace("/v1", "/deepseek/v1");
        let llm = OpenAiCompatibleLlm::new(url, "t".into(), "p".into(), 5000);
        let _ = llm.complete(crate::llm::LlmInput {
            blocks: vec![],
            user_text: "x".into(),
            granted_operations: vec!["time.now".to_string()],
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
            ns.contains(&"time.now"),
            "passthrough sends canonical even with deepseek URL: {ns:?}"
        );
        assert!(
            !ns.contains(&"fn_0"),
            "fn_N must NOT be used in passthrough"
        );
        Ok(())
    }

    // ===== Real primary 429 → fallback with explicit IndexedMapping config =====
    #[test]
    fn primary_429_triggers_indexed_fallback_two_rounds() -> Result<()> {
        let p = Primary429::new();
        let fb = Capture::new(vec![
            json!({"model":"s","choices":[{"message":{"content":"","tool_calls":[{"id":"fb1","type":"function","function":{"name":"fn_0","arguments":"{}"}}]}}]}),
            json!({"model":"s","choices":[{"message":{"content":"done"}}]}),
        ]);
        let mut c = cfg();
        c.extra_allowed_operations = vec!["time.now".to_string()];
        c.fallback_tool_name_indexed = true;
        c.fallback_openai_base_url = fb.url();
        c.fallback_openai_api_key = "test".into();
        c.fallback_model = "fs".into();
        let llm = OpenAiCompatibleLlm::new(p.url(), "t".into(), "p".into(), 5000)
            .with_indexed_fallback(fb.url(), "test".into(), "fs".into());
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
        assert_eq!(p.hits(), 2, "primary hit 2 times, both 429");
        let ns: Vec<&str> = reqs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert!(ns.contains(&"fn_0"), "fallback tools use fn_N");
        assert!(!ns.iter().any(|n| n.contains("time.now")));
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
                        && e.payload.get("operation").and_then(Value::as_str) == Some("time.now")
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
                    && e.payload.get("operation").and_then(Value::as_str) == Some("time.now")
            })
            .expect("approved time.now");
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
        // Round-2 system/context (messages[0].content) contains the textual
        // ToolResult block (the project uses ContextBlock::ToolResult, NOT
        // OpenAI role=tool messages).
        let round2_ctx = reqs[1]["messages"][0]["content"].as_str().unwrap_or("");
        assert!(
            round2_ctx.contains("time.now"),
            "round-2 context contains ToolResult for time.now"
        );
        assert!(
            round2_ctx.contains("succeeded"),
            "round-2 context shows status: succeeded"
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

    // ===== §3: production config wiring (build_llm_from_config) — 4 combos =====
    //
    // Exercises KernelConfig → build_llm_from_config → ModelEndpoint wiring,
    // the SAME path delivery.rs uses. No HTTP needed: the indexed flags are
    // inspected directly. Primary and fallback are independent.

    fn wired_cfg() -> crate::config::KernelConfig {
        let mut c = cfg();
        c.openai_base_url = "http://primary/v1".into();
        c.openai_api_key = "k".into();
        c.model = "m".into();
        c.fallback_openai_base_url = "http://fallback/v1".into();
        c.fallback_openai_api_key = "fk".into();
        c.fallback_model = "fm".into();
        c
    }
    use crate::llm::ToolNameMode;
    fn primary_indexed(llm: &crate::llm::OpenAiCompatibleLlm) -> bool {
        matches!(llm.primary.tool_name_mode, ToolNameMode::IndexedMapping(_))
    }
    fn fallback_indexed(llm: &crate::llm::OpenAiCompatibleLlm) -> bool {
        llm.fallback
            .as_ref()
            .map(|e| matches!(e.tool_name_mode, ToolNameMode::IndexedMapping(_)))
            .unwrap_or(false)
    }

    #[test]
    fn config_both_passthrough_default() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = false;
        c.fallback_tool_name_indexed = false;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(!primary_indexed(&llm), "primary passthrough");
        assert!(!fallback_indexed(&llm), "fallback passthrough");
    }

    #[test]
    fn config_primary_indexed_only() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = true;
        c.fallback_tool_name_indexed = false;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(primary_indexed(&llm), "primary indexed");
        assert!(!fallback_indexed(&llm), "fallback still passthrough");
    }

    #[test]
    fn config_fallback_indexed_only() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = false;
        c.fallback_tool_name_indexed = true;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(!primary_indexed(&llm), "primary still passthrough");
        assert!(fallback_indexed(&llm), "fallback indexed");
    }

    #[test]
    fn config_both_indexed_independent() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = true;
        c.fallback_tool_name_indexed = true;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(primary_indexed(&llm));
        assert!(fallback_indexed(&llm));
    }

    #[test]
    fn config_fallback_indexed_without_endpoint_does_not_create_one() {
        // If fallback_tool_name_indexed=true but no fallback URL is configured,
        // build_llm_from_config must NOT create a fallback endpoint.
        let mut c = cfg();
        c.fallback_openai_base_url = String::new();
        c.fallback_tool_name_indexed = true;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(
            !fallback_indexed(&llm),
            "no endpoint created from empty URL"
        );
    }

    // ===== §4: freeze env_bool parsing (pure function) =====

    #[test]
    fn env_bool_accepts_common_truthy_values() {
        use crate::config::parse_env_bool_value;
        for v in ["true", "TRUE", "True", "1", "yes", "YES", "on", "on "] {
            assert!(parse_env_bool_value(v, false), "truthy: {v:?}");
        }
        for v in ["false", "FALSE", "0", "no", "off", "OFF"] {
            assert!(!parse_env_bool_value(v, true), "falsy: {v:?}");
        }
    }

    #[test]
    fn env_bool_unparsable_falls_back_safely() {
        use crate::config::parse_env_bool_value;
        // Invalid values fall back to the provided default — they do NOT silently
        // enable indexed mapping (which could mask a deployment misconfiguration).
        for v in ["", "maybe", "2", "y", "n", "yes/no"] {
            assert_eq!(
                parse_env_bool_value(v, false),
                false,
                "invalid → false default: {v:?}"
            );
            assert_eq!(
                parse_env_bool_value(v, true),
                true,
                "invalid → true default: {v:?}"
            );
        }
    }
}
