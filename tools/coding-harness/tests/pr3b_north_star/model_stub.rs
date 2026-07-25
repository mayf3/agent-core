use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use super::support::{read_http_request, write_http_response};

pub struct ModelStub {
    pub port: u16,
    responses: Arc<Mutex<VecDeque<Value>>>,
}

impl ModelStub {
    pub fn start(responses: Vec<Value>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let queued = Arc::clone(&responses);
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let result = (|| -> Result<()> {
                    let (request_line, headers, _) = read_http_request(&mut stream)?;
                    if request_line != "POST /v1/chat/completions HTTP/1.1"
                        || headers.get("authorization").map(String::as_str)
                            != Some("Bearer pr3b-model-key")
                    {
                        return write_http_response(&mut stream, 401, json!({"error":"denied"}));
                    }
                    let response = queued
                        .lock()
                        .expect("model response queue")
                        .pop_front()
                        .unwrap_or_else(|| json!({"error":"unexpected_model_call"}));
                    let status = if response.get("error").is_some() {
                        500
                    } else {
                        200
                    };
                    write_http_response(&mut stream, status, response)
                })();
                if let Err(error) = result {
                    eprintln!("model stub request failed: {error}");
                }
            }
        });
        Ok(Self { port, responses })
    }

    pub fn assert_exhausted(&self) -> Result<()> {
        let remaining = self.responses.lock().expect("model response queue").len();
        if remaining != 0 {
            bail!("model stub has {remaining} unused response(s)");
        }
        Ok(())
    }
}

pub fn tool_call(call_id: &str, operation: &str, arguments: Value) -> Value {
    json!({
        "model": "pr3b-model-stub",
        "choices": [{
            "message": {
                "content": "",
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": operation,
                        "arguments": arguments.to_string()
                    }
                }]
            }
        }]
    })
}

pub fn text_reply(text: &str) -> Value {
    json!({
        "model": "pr3b-model-stub",
        "choices": [{"message": {"content": text}}]
    })
}
