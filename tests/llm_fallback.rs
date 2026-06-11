use agent_core_kernel::llm::{LlmClient, LlmInput, OpenAiCompatibleLlm};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

#[test]
fn fallback_endpoint_is_used_after_primary_http_error() -> Result<()> {
    let primary = serve_once(400, json!({ "error": { "message": "bad model" } }))?;
    let fallback = serve_once(
        200,
        json!({
            "model": "deepseek-v4-flash",
            "choices": [{ "message": { "content": "fallback ok" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        }),
    )?;
    let llm = OpenAiCompatibleLlm::new(
        primary,
        "primary-key".to_string(),
        "bad-primary".to_string(),
        2_000,
    )
    .with_fallback(
        fallback,
        "fallback-key".to_string(),
        "deepseek-v4-flash".to_string(),
    );

    let output = llm.complete(LlmInput {
        blocks: vec![],
        user_text: "hello".to_string(),
    })?;

    assert_eq!(output.model, "deepseek-v4-flash");
    assert_eq!(output.content, "fallback ok");
    assert_eq!(
        output
            .journal_payload
            .pointer("/fallback/used")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output
            .journal_payload
            .pointer("/fallback/primary_error_category")
            .and_then(Value::as_str),
        Some("model_http_400")
    );
    Ok(())
}

fn serve_once(status: u16, body: Value) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_http_request(&mut stream);
            let body = body.to_string();
            let status_text = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Ok(format!("http://{addr}/v1"))
}

fn read_http_request(stream: &mut TcpStream) -> Result<()> {
    let mut buffer = [0_u8; 2048];
    let _ = stream.read(&mut buffer)?;
    Ok(())
}
