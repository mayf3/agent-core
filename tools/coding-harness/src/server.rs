use crate::config::CodingConfig;
use crate::{capability, tasks, workspace};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

const MAX_BODY: usize = 2_200_000;

pub fn serve(listener: TcpListener, config: Arc<CodingConfig>) {
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let c = Arc::clone(&config);
                std::thread::spawn(move || handle(s, &c));
            }
            Err(e) => eprintln!("coding_harness accept: {e}"),
        }
    }
}

fn handle(mut stream: TcpStream, config: &CodingConfig) {
    let mut buf = Vec::with_capacity(8192);
    let header_end = loop {
        let mut chunk = [0u8; 1024];
        let n = match stream.read(&mut chunk) {
            Ok(0) => {
                let _ = respond_error(&mut stream, 400, "connection_closed");
                return;
            }
            Ok(n) => n,
            Err(_) => {
                let _ = respond_error(&mut stream, 400, "read_error");
                return;
            }
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 65536 {
            let _ = respond_error(&mut stream, 413, "headers_too_large");
            return;
        }
    };

    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = match parse_cl(&headers) {
        Ok(Some(n)) => n,
        Ok(None) => {
            let _ = respond_error(&mut stream, 400, "missing_content_length");
            return;
        }
        Err(e) => {
            let _ = respond_error(&mut stream, 400, e);
            return;
        }
    };
    if content_length > MAX_BODY {
        let _ = respond_error(&mut stream, 413, "body_too_large");
        return;
    }
    if has_chunked(&headers) {
        let _ = respond_error(&mut stream, 400, "chunked_not_supported");
        return;
    }

    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; (content_length - body.len()).min(65536)];
        let n = match stream.read(&mut chunk) {
            Ok(0) => {
                let _ = respond_error(&mut stream, 400, "body_truncated");
                return;
            }
            Ok(n) => n,
            Err(_) => {
                let _ = respond_error(&mut stream, 400, "body_read_error");
                return;
            }
        };
        body.extend_from_slice(&chunk[..n]);
    }

    let body_str = match String::from_utf8(body) {
        Ok(s) => s,
        Err(_) => {
            let _ = respond_error(&mut stream, 400, "invalid_utf8");
            return;
        }
    };
    let parsed: Value = match serde_json::from_str(&body_str) {
        Ok(v) => v,
        Err(_) => {
            let _ = respond_error(&mut stream, 400, "invalid_json");
            return;
        }
    };

    if parsed
        .get("protocol_version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        != "external-harness-v1"
    {
        let _ = respond_error(&mut stream, 400, "unsupported_protocol");
        return;
    }

    let op = parsed
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args = parsed.get("arguments").cloned().unwrap_or(json!({}));

    // Dispatch to handler, get structured response (already has ok/error_code).
    let handler_result = dispatch(config, op, &args);
    let body_bytes = serde_json::to_vec(&handler_result).unwrap_or_default();

    // Write HTTP 200 with the handler response as body.
    let reason = "OK";
    let resp = format!(
        "HTTP/1.1 200 {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_bytes.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.write_all(&body_bytes);
}

/// Send a service-level error response (protocol errors, not operation errors).
fn respond_error(stream: &mut TcpStream, status: u16, error_code: &str) {
    let body = json!({"protocol_version":"external-harness-v1","ok":false,"error_code":error_code});
    let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
    let reason = if status == 200 { "OK" } else { "Error" };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_bytes.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.write_all(&body_bytes);
}

fn dispatch(config: &CodingConfig, operation: &str, args: &Value) -> Value {
    // North Star controlled development path.  This intentionally runs
    // before generic workspace dispatch: callers cannot select a workspace,
    // backend, model, or arbitrary objective for calculator-v0.
    if operation == "external.coding_task_submit" && args.get("schema_version").is_some() {
        return crate::calculator_generator::handle_submit(&config.artifact_root, args);
    }
    // HCR acceptance is another narrow control operation. It resolves the
    // candidate only below artifact_root and must not be forced through the
    // generic caller-selected workspace path.
    if operation == "external.coding_hcr_accept" {
        return crate::hcr::acceptance::handle_accept(&config.artifact_root, args);
    }

    let is_task_op = operation == "external.coding_task_status";
    let available_ids: Vec<String> = config.workspaces.keys().cloned().collect();
    let ws_id = if is_task_op {
        None
    } else {
        match args.get("workspace_id").and_then(Value::as_str) {
            Some(id) => Some(id.to_string()),
            None => {
                return structured_err("missing_workspace_id", &available_ids, &["workspace_id"])
            }
        }
    };

    let root = if is_task_op {
        None
    } else {
        let id = ws_id.as_ref().unwrap();
        match config.root_for(id) {
            Some(r) => {
                let perm = config.perm_for(id).unwrap();
                let needs_exec = operation == "external.coding_workspace_exec"
                    || operation == "external.coding_task_submit"
                    || operation == "external.coding_hcr_exec";
                let needs_write = operation == "external.coding_workspace_write";
                if needs_exec && !perm.exec {
                    return err_value("exec_not_permitted");
                }
                if operation == "external.coding_task_submit" {
                    if let Some(backend) = args.get("backend").and_then(Value::as_str) {
                        match backend {
                            "opencode" if !perm.opencode => {
                                return err_value("opencode_not_permitted")
                            }
                            "opencode" if !perm.network => {
                                return err_value("network_required_for_opencode")
                            }
                            _ => {}
                        }
                    }
                }
                if needs_write && !perm.write {
                    return err_value("write_not_permitted");
                }
                if !needs_exec && !needs_write && !is_task_op && !perm.read {
                    return err_value("read_not_permitted");
                }
                Some(r.clone())
            }
            None => return structured_err("unknown_workspace_id", &available_ids, &[]),
        }
    };

    // ── HCR execution dispatch ──
    // Uses profile-based security model, not workspace permission booleans.
    if operation == "external.coding_hcr_exec" {
        let ws_id = ws_id.as_ref().unwrap();
        let profile_id = args
            .get("hcr_profile_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let request_token = args.get("hcr_token").and_then(Value::as_str).unwrap_or("");

        // Token validation: HCR profile requires matching token
        if config.hcr_token.is_empty() || request_token != config.hcr_token {
            return err_value("hcr_token_required");
        }

        let profile = match config.hcr_profiles.get(profile_id) {
            Some(p) => p,
            None => return err_value("hcr_profile_not_found"),
        };

        // Verify workspace_id matches profile binding
        if profile.workspace_id != *ws_id {
            return err_value("hcr_workspace_mismatch");
        }

        return workspace::handle_hcr_exec(root.as_ref().unwrap(), args, profile);
    }

    match operation {
        "external.coding_workspace_list" => workspace::handle_list(root.as_ref().unwrap(), args),
        "external.coding_workspace_read" => workspace::handle_read(root.as_ref().unwrap(), args),
        "external.coding_workspace_write" => workspace::handle_write(root.as_ref().unwrap(), args),
        "external.coding_workspace_exec" => {
            let perm = config.perm_for(ws_id.as_ref().unwrap()).unwrap();
            workspace::handle_exec(root.as_ref().unwrap(), args, perm)
        }
        "external.coding_task_submit" => {
            let ws = ws_id.as_ref().unwrap();
            let objective = args.get("objective").and_then(Value::as_str).unwrap_or("");
            let acceptance = args
                .get("acceptance_criteria")
                .cloned()
                .unwrap_or(Value::Null);
            let backend = args
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("fake");
            let model = args.get("model").and_then(Value::as_str);
            let wr = root.as_ref().map(|r| r.to_string_lossy().to_string());
            tasks::submit_task(ws, objective, &acceptance, backend, wr.as_deref(), model)
        }
        "external.coding_task_status" => {
            let task_id = args.get("task_id").and_then(Value::as_str).unwrap_or("");
            tasks::get_status(task_id)
        }
        "external.coding_capability_propose" => {
            capability::handle_propose(root.as_ref().unwrap(), args, config)
        }
        _ => err_value("unknown_operation"),
    }
}

fn parse_cl(headers: &str) -> Result<Option<usize>, &'static str> {
    let mut found: Option<usize> = None;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                let trimmed = value.trim();
                if trimmed.is_empty() || trimmed.starts_with('+') || trimmed.starts_with('-') {
                    return Err("invalid_content_length");
                }
                let n: usize = trimmed.parse().map_err(|_| "invalid_content_length")?;
                match found {
                    Some(p) if p != n => return Err("conflicting_content_length"),
                    _ => found = Some(n),
                }
            }
        }
    }
    Ok(found)
}

fn has_chunked(headers: &str) -> bool {
    headers
        .lines()
        .filter_map(|l| l.split_once(':'))
        .any(|(n, v)| {
            n.eq_ignore_ascii_case("transfer-encoding") && v.trim().eq_ignore_ascii_case("chunked")
        })
}

fn err_value(code: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":code})
}

/// Structured error with retryable hint and details for model recovery.
fn structured_err(code: &str, available_ids: &[String], missing: &[&str]) -> Value {
    let mut details = json!({});
    if !missing.is_empty() {
        details["missing_fields"] = json!(missing);
    }
    if !available_ids.is_empty() {
        details["available_workspace_ids"] = json!(available_ids);
    }
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
        "retryable": true,
        "details": details,
    })
}
