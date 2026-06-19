mod common;

use agent_core_kernel::domain::*;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::OpenAiCompatibleLlm;
use agent_core_kernel::runtime::Runtime;
use anyhow::Result;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

fn read_http_request_body(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = stream.read(&mut b)?;
        if n == 0 {
            anyhow::bail!("eof");
        }
        buf.push(b[0]);
        if buf.len() >= 4 && buf[buf.len() - 4..] == [b'\r', b'\n', b'\r', b'\n'] {
            break;
        }
    }
    let header = String::from_utf8_lossy(&buf);
    let content_len: usize = header
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        let mut offset = 0;
        while offset < content_len {
            let n = stream.read(&mut body[offset..])?;
            if n == 0 {
                anyhow::bail!("eof");
            }
            offset += n;
        }
    }
    Ok(String::from_utf8(body)?)
}

struct StubProvider {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StubProvider {
    fn start(rounds: &[&str]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let r = Arc::clone(&requests);
        let bodies: Vec<String> = rounds.iter().map(|s| s.to_string()).collect();
        let handle = thread::spawn(move || {
            for body in &bodies {
                let (mut stream, _) = match listener.accept() {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                if let Some(body_val) = read_http_request_body(&mut stream)
                    .ok()
                    .and_then(|b| serde_json::from_str::<Value>(&b).ok())
                {
                    r.lock().unwrap().push(body_val);
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        Self {
            port,
            requests,
            handle: Some(handle),
        }
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
}

impl Drop for StubProvider {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn setup(
    server: &StubProvider,
    extra_ops: Vec<&str>,
) -> Result<(JournalStore, Gateway, Runtime<OpenAiCompatibleLlm>)> {
    let mut config = common::test_config();
    config.openai_base_url = server.url();
    config.openai_api_key = "stub-key".into();
    config.model = "stub".into();
    config.extra_allowed_operations = extra_ops.iter().map(|s| s.to_string()).collect();
    let journal = JournalStore::in_memory()?;
    let gateway = Gateway::new(config.clone());
    let llm = OpenAiCompatibleLlm::new(
        config.openai_base_url.clone(),
        config.openai_api_key.clone(),
        config.model.clone(),
        5000,
    );
    let runtime = Runtime::new(config, llm);
    Ok((journal, gateway, runtime))
}

#[test]
fn stub_provider_completes_tool_loop() -> Result<()> {
    let server = StubProvider::start(&[
        r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call_stub_1","type":"function","function":{"name":"time.now","arguments":"{}"}}]}}],"model":"stub"}"#,
        r#"{"choices":[{"message":{"content":"The current time was retrieved."}}],"model":"stub"}"#,
    ]);
    thread::sleep(std::time::Duration::from_millis(100));
    let (journal, gateway, runtime) = setup(&server, vec!["system.status", "time.now"])?;
    let envelope = gateway.cli_ingress("what time is it".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert_eq!(outcome.output, "The current time was retrieved.");

    let events = journal.events()?;
    let re: Vec<_> = events
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&outcome.run_id))
        .collect();
    assert_eq!(
        re.iter()
            .filter(|e| e.kind == JournalEventKind::LlmCompleted)
            .count(),
        2
    );
    assert_eq!(
        re.iter()
            .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
            .count(),
        1
    );

    let captured = server.requests.lock().unwrap();
    assert!(captured.len() >= 2);
    let tools = captured[0].get("tools").and_then(Value::as_array).unwrap();
    assert_eq!(tools.len(), 3);
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"time.now"));
    assert!(names.contains(&"session.recall_recent"));
    assert!(names.contains(&"system.status"));

    let req2 = &captured[1];
    let sys = req2
        .pointer("/messages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .unwrap();
    let c = sys.get("content").and_then(Value::as_str).unwrap_or("");
    assert!(c.contains("ToolResult") && c.contains("time.now"));
    Ok(())
}

#[test]
fn malformed_json_arguments_no_receipt_and_nonempty_reply() -> Result<()> {
    let server = StubProvider::start(&[
        r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"bad_1","type":"function","function":{"name":"time.now","arguments":"{invalid json}"}}]}}],"model":"stub"}"#,
        r#"{"choices":[{"message":{"content":"I could not parse the tool arguments."}}],"model":"stub"}"#,
    ]);
    thread::sleep(std::time::Duration::from_millis(100));
    let (journal, gateway, runtime) = setup(&server, vec!["time.now"])?;
    let envelope = gateway.cli_ingress("what time is it".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert!(!outcome.output.is_empty());
    assert!(outcome.output.contains("parse") || outcome.output.contains("malformed"));

    let events = journal.events()?;
    let re: Vec<_> = events
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&outcome.run_id))
        .collect();
    let receipts = re
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .count();
    assert_eq!(receipts, 0);
    Ok(())
}

#[test]
fn non_object_arguments_no_receipt_and_nonempty_reply() -> Result<()> {
    let server = StubProvider::start(&[
        r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"bad_2","type":"function","function":{"name":"time.now","arguments":"\"just a string\""}}]}}],"model":"stub"}"#,
        r#"{"choices":[{"message":{"content":"String arguments are not valid."}}],"model":"stub"}"#,
    ]);
    thread::sleep(std::time::Duration::from_millis(100));
    let (journal, gateway, runtime) = setup(&server, vec!["time.now"])?;
    let envelope = gateway.cli_ingress("what time is it".into())?;
    let event = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, event)?;
    assert!(!outcome.output.is_empty());
    assert!(
        outcome.output.contains("object")
            || outcome.output.contains("string")
            || outcome.output.contains("arguments")
    );

    let events = journal.events()?;
    let re: Vec<_> = events
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&outcome.run_id))
        .collect();
    let receipts = re
        .iter()
        .filter(|e| e.kind == JournalEventKind::ReceiptReceived)
        .count();
    assert_eq!(receipts, 0);
    Ok(())
}
