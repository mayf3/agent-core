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
        let _ = r.deliver(
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
        assert_eq!(tnp(JournalEventKind::InvocationApproved), 0);
        assert_eq!(tnp(JournalEventKind::ReceiptReceived), 0);
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
        assert_eq!(
            ev.iter()
                .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
                .count(),
            1
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
}
