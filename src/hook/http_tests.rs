use crate::hook::*;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

const PROVIDER_ID: &str = "http-test-provider";
const SECRET: &str = "http-test-secret";

fn request() -> ContextHookRequest {
    let immutable_refs = vec![ImmutableArtifactRef::new("required", b"required")];
    ContextHookRequest {
        request_id: "request-1".into(),
        candidate: CandidateInputRef {
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            scope_digest: crate::capabilities::store::Sha256Digest::compute(b"scope")
                .as_str()
                .into(),
            artifact: OpaqueArtifactRef::new("application/test", b"candidate"),
            immutable_refs_digest: digest_immutable_refs(&immutable_refs),
            immutable_refs,
        },
    }
}

fn config(port: u16) -> HookConfig {
    HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: format!("http://127.0.0.1:{port}/context.prepare.v0"),
        },
        timeout_ms: 5_000,
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 1024 * 1024,
        failure_mode: HookFailureMode::FailClosed,
        provider_id: PROVIDER_ID.into(),
        shared_secret: SECRET.into(),
    }
}

fn spawn_authenticated_provider() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request_bytes = read_request(&mut stream);
        let request_text = String::from_utf8(request_bytes).unwrap();
        let (headers, body) = request_text.split_once("\r\n\r\n").unwrap();
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {SECRET}"))));
        let envelope: Value = serde_json::from_str(body).unwrap();
        let request: ContextHookRequest =
            serde_json::from_value(envelope["payload"].clone()).unwrap();
        let response = ContextHookResponse {
            run_id: request.candidate.run_id.clone(),
            session_id: request.candidate.session_id.clone(),
            scope_digest: request.candidate.scope_digest.clone(),
            candidate_digest: request.candidate.artifact.digest.clone(),
            immutable_refs: request.candidate.immutable_refs.clone(),
            immutable_refs_digest: request.candidate.immutable_refs_digest.clone(),
            artifacts: vec![request.candidate.artifact],
        };
        let proof = compute_provider_proof(
            SECRET,
            &response.authentication_message(PROVIDER_ID, &request.request_id),
        )
        .unwrap();
        let body = serde_json::to_string(&HookResponseEnvelope {
            request_id: request.request_id,
            hook: HookKind::ContextPrepareV0,
            timestamp: Utc::now(),
            payload: serde_json::to_value(response).unwrap(),
        })
        .unwrap();
        write_response(
            &mut stream,
            200,
            &[("X-Agent-Core-Provider-Proof", proof.as_str())],
            &body,
        );
    });
    port
}

fn spawn_static(body: String, proof: Option<&'static str>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_request(&mut stream);
        let headers = proof
            .map(|value| vec![("X-Agent-Core-Provider-Proof", value)])
            .unwrap_or_default();
        write_response(&mut stream, 200, &headers, &body);
    });
    port
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        bytes.extend_from_slice(&buffer[..count]);
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if bytes.len() >= header_end + 4 + length {
            bytes.truncate(header_end + 4 + length);
            return bytes;
        }
    }
}

fn write_response(stream: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &str) {
    let extra = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

#[test]
fn authenticated_http_binding_returns_bound_artifact() {
    let port = spawn_authenticated_provider();
    let request = request();
    let response = HttpHookClient::new()
        .call_context(&request, &config(port))
        .unwrap();
    assert_eq!(response.provider_id, PROVIDER_ID);
    assert_eq!(response.request_id, request.request_id);
    assert_eq!(response.response.artifacts[0], request.candidate.artifact);
}

#[test]
fn missing_provider_proof_is_rejected() {
    let port = spawn_static(json!({}).to_string(), None);
    let error = HttpHookClient::new()
        .call_context(&request(), &config(port))
        .unwrap_err();
    assert!(error.to_string().contains("provider_proof_missing"));
}

#[test]
fn response_size_limit_is_enforced_before_json_parsing() {
    let port = spawn_static("x".repeat(256), Some("00"));
    let mut config = config(port);
    config.max_response_bytes = 32;
    let error = HttpHookClient::new()
        .call_context(&request(), &config)
        .unwrap_err();
    assert!(error.to_string().contains("response_too_large"));
}
