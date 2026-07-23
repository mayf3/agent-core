use crate::deployment::UpstreamRead;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

pub fn read(upstream: &UpstreamRead) -> Result<Value, String> {
    let base = std::env::var("CAPABILITY_HOST_DEPLOYMENT_HARNESS_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7400".into());
    let token = std::env::var("CAPABILITY_HOST_DEPLOYMENT_HARNESS_READ_TOKEN")
        .map_err(|_| "deployment_harness_read_token_missing".to_string())?;
    read_with_config(upstream, &base, &token)
}

fn read_with_config(upstream: &UpstreamRead, base: &str, token: &str) -> Result<Value, String> {
    validate_token(&token)?;
    let (address, base_path) = parse_loopback_url(base)?;
    if base_path != "/" {
        return Err("deployment_harness_url_invalid".into());
    }
    let status = get_json(
        address,
        &format!("/v1/components/{}", upstream.component_id),
        Some(&token),
    )?;
    if status.get("ok").and_then(Value::as_bool) != Some(true)
        || status.get("health_status").and_then(Value::as_str) != Some("ready")
    {
        return Err("upstream_component_not_ready".into());
    }
    let endpoint = status
        .get("endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| "upstream_endpoint_missing".to_string())?;
    let (component_address, _) = parse_loopback_url(endpoint)?;
    get_json(component_address, &upstream.path, None)
}

fn get_json(address: SocketAddr, path: &str, token: Option<&str>) -> Result<Value, String> {
    if !path.starts_with('/') || path.contains(['\r', '\n']) {
        return Err("upstream_path_invalid".into());
    }
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))
        .map_err(|_| "upstream_connect_failed".to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .and_then(|_| stream.set_write_timeout(Some(Duration::from_millis(500))))
        .map_err(|_| "upstream_timeout_setup_failed".to_string())?;
    let authorization = token
        .map(|value| format!("Authorization: Bearer {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\n{authorization}Connection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "upstream_write_failed".to_string())?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut response)
        .map_err(|_| "upstream_read_failed".to_string())?;
    if response.len() > MAX_RESPONSE_BYTES {
        return Err("upstream_response_too_large".into());
    }
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "upstream_http_invalid".to_string())?;
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|_| "upstream_http_invalid".to_string())?;
    if !headers.starts_with("HTTP/1.1 200 ") {
        return Err("upstream_http_failed".into());
    }
    serde_json::from_slice(&response[header_end + 4..])
        .map_err(|_| "upstream_json_invalid".to_string())
}

fn parse_loopback_url(value: &str) -> Result<(SocketAddr, String), String> {
    let remainder = value
        .strip_prefix("http://")
        .ok_or_else(|| "upstream_url_invalid".to_string())?;
    let (authority, suffix) = remainder.split_once('/').unwrap_or((remainder, ""));
    if authority.contains('@') {
        return Err("upstream_url_invalid".into());
    }
    let address: SocketAddr = authority
        .parse()
        .map_err(|_| "upstream_url_invalid".to_string())?;
    if !loopback(address.ip()) {
        return Err("upstream_url_not_loopback".into());
    }
    Ok((address, format!("/{suffix}")))
}

fn validate_token(value: &str) -> Result<(), String> {
    if value.len() < 32
        || value.len() > 512
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err("deployment_harness_read_token_invalid".into());
    }
    Ok(())
}

fn loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn trusted_discovery_reads_failure_viewer_state() {
        let component = TcpListener::bind("127.0.0.1:0").unwrap();
        let component_addr = component.local_addr().unwrap();
        let component_thread = thread::spawn(move || {
            respond_once(
                component,
                &json_body(r#"{"rendered":{"component_id":"failure-viewer","failure_count":1}}"#),
                Some("GET /api/state "),
            )
        });
        let deployment = TcpListener::bind("127.0.0.1:0").unwrap();
        let deployment_addr = deployment.local_addr().unwrap();
        let status = format!(
            r#"{{"ok":true,"health_status":"ready","endpoint":"http://{component_addr}"}}"#
        );
        let deployment_thread = thread::spawn(move || {
            respond_once(
                deployment,
                &json_body(&status),
                Some("GET /v1/components/failure-viewer "),
            )
        });
        let upstream = UpstreamRead {
            component_id: "failure-viewer".into(),
            method: "GET".into(),
            path: "/api/state".into(),
        };
        let value = read_with_config(
            &upstream,
            &format!("http://{deployment_addr}"),
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        assert_eq!(value["rendered"]["component_id"], "failure-viewer");
        assert_eq!(value["rendered"]["failure_count"], 1);
        deployment_thread.join().unwrap();
        component_thread.join().unwrap();
    }

    #[test]
    fn discovery_and_component_endpoints_must_be_loopback() {
        assert_eq!(
            parse_loopback_url("http://192.0.2.10:7400").unwrap_err(),
            "upstream_url_not_loopback"
        );
        assert!(parse_loopback_url("https://127.0.0.1:7400").is_err());
    }

    fn respond_once(listener: TcpListener, response: &str, expected_request: Option<&str>) {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        if let Some(expected) = expected_request {
            assert!(request.starts_with(expected), "request was {request:?}");
        }
        stream.write_all(response.as_bytes()).unwrap();
    }

    fn json_body(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }
}
