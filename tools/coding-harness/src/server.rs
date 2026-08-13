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
    let bearer = parse_bearer(&headers);
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
    let control_token = std::env::var("CODING_HARNESS_CONTROL_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let execution_token = std::env::var("AGENT_CORE_CAPABILITY_HOST_EXECUTION_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty());
    // Two distinct keys, kept separate on purpose:
    //   - external.coding_task_submit (control surface)      -> CODING_HARNESS_CONTROL_TOKEN
    //   - external.execution_task (ordinary execution)       -> AGENT_CORE_CAPABILITY_HOST_EXECUTION_TOKEN
    // The Kernel's generic external dispatch sends the execution token,
    // so execution_task authenticates with it. Fail-closed: unset or
    // mismatched tokens are rejected.
    if !matches_expected_token(
        op,
        execution_token.as_deref(),
        control_token.as_deref(),
        bearer.as_deref(),
    ) {
        let _ = respond_error(&mut stream, 401, "unauthorized");
        return;
    }
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
    // Generic self-evolution control path. The DevelopmentRequest selects a
    // catalogued profile; callers cannot select an arbitrary command/backend.
    if operation == "external.coding_task_submit" && args.get("development_request").is_some() {
        return crate::self_evolution::handle_submit(&config.artifact_root, args);
    }
    if operation == "external.coding_self_evolution_discover" {
        return json!({
            "protocol_version": "external-harness-v1",
            "ok": true,
            "result": crate::self_evolution::discovery(),
        });
    }
    if operation == "external.coding_hcr_accept" || operation == "external.coding_hcr_exec" {
        return err_value("hcr_active_workflow_retired");
    }
    if operation == "external.component_build" {
        return crate::component_build::handle(args, config);
    }
    if operation == "external.execution_task" {
        let canonical_root = match resolve_execution_task_root(
            config,
            args.get("root").and_then(Value::as_str),
        ) {
            Ok(root) => root,
            Err(error) => return err_value(error),
        };
        let objective = match args.get("objective").and_then(Value::as_str) {
            Some(objective) if !objective.trim().is_empty() => objective,
            _ => return err_value("missing_objective"),
        };
        let root = canonical_root.to_string_lossy().to_string();
        return tasks::submit_task("", objective, &Value::Null, "opencode", Some(&root), None);
    }

    let is_task_op = operation == "external.coding_task_status";
    // Workspace-scoped coding tasks (opencode backend) use ONLY the
    // dedicated high-level operation. external.coding_task_submit is
    // exclusively the DevelopmentRequest pipeline (handled above when
    // `development_request` is present) — no workspace alias.
    let is_workspace_task = operation == "external.coding_workspace_task";
    // workspace_task must REALLY run opencode — never the fake backend.
    // Validated before any workspace resolution so it cannot fake-succeed.
    if operation == "external.coding_workspace_task" {
        if let Some(backend) = args.get("backend").and_then(Value::as_str) {
            if backend != "opencode" {
                return err_value("backend_must_be_opencode");
            }
        }
    }
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
                let needs_exec = operation == "external.coding_workspace_exec" || is_workspace_task;
                let needs_write = operation == "external.coding_workspace_write";
                if needs_exec && !perm.exec {
                    return err_value("exec_not_permitted");
                }
                if is_workspace_task {
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

    match operation {
        "external.coding_workspace_list" => workspace::handle_list(root.as_ref().unwrap(), args),
        "external.coding_workspace_read" => workspace::handle_read(root.as_ref().unwrap(), args),
        "external.coding_workspace_write" => workspace::handle_write(root.as_ref().unwrap(), args),
        "external.coding_workspace_exec" => {
            let perm = config.perm_for(ws_id.as_ref().unwrap()).unwrap();
            workspace::handle_exec(root.as_ref().unwrap(), args, perm)
        }
        "external.coding_workspace_task" => {
            // Missing backend defaults to opencode (validated above).
            let ws = ws_id.as_ref().unwrap();
            let objective = args.get("objective").and_then(Value::as_str).unwrap_or("");
            let acceptance = args
                .get("acceptance_criteria")
                .cloned()
                .unwrap_or(Value::Null);
            let model = args.get("model").and_then(Value::as_str);
            let wr = root.as_ref().map(|r| r.to_string_lossy().to_string());
            tasks::submit_task(ws, objective, &acceptance, "opencode", wr.as_deref(), model)
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

fn resolve_execution_task_root(
    config: &CodingConfig,
    requested_root: Option<&str>,
) -> Result<std::path::PathBuf, &'static str> {
    let requested_root = requested_root.ok_or("missing_root")?;
    if !std::path::Path::new(requested_root).is_absolute() {
        return Err("root_not_absolute");
    }
    let canonical_root = std::fs::canonicalize(requested_root).map_err(|_| "root_not_found")?;
    if !canonical_root.is_dir() {
        return Err("root_not_found");
    }
    let permission = config
        .workspaces
        .values()
        .find(|entry| entry.root == canonical_root)
        .map(|entry| &entry.perm)
        .ok_or("root_not_allowed")?;
    if !permission.exec || !permission.opencode || !permission.network {
        return Err("task_not_permitted");
    }
    Ok(canonical_root)
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

fn parse_bearer(headers: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("authorization") {
            return None;
        }
        value
            .trim()
            .strip_prefix("Bearer ")
            .filter(|token| !token.is_empty())
            .map(str::to_string)
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

/// Whether `bearer` satisfies the token required for `op`. Fail-closed:
/// an unset expected token or a missing/mismatched bearer is rejected.
/// Only task operations authenticate; everything else passes through.
fn matches_expected_token(
    op: &str,
    execution_token: Option<&str>,
    control_token: Option<&str>,
    bearer: Option<&str>,
) -> bool {
    let expected = match op {
        "external.execution_task" => execution_token,
        "external.coding_workspace_task" => execution_token,
        "external.coding_task_submit" => control_token,
        "external.component_build" => execution_token,
        _ => return true,
    };
    match (expected, bearer) {
        (Some(e), Some(b)) => e == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXEC: &str = "exec-token";
    const CONTROL: &str = "control-token";

    #[test]
    fn workspace_task_authenticates_with_execution_token() {
        assert!(matches_expected_token(
            "external.execution_task",
            Some(EXEC),
            Some(CONTROL),
            Some(EXEC)
        ));
        assert!(matches_expected_token(
            "external.coding_workspace_task",
            Some(EXEC),
            Some(CONTROL),
            Some(EXEC)
        ));
        // wrong token (control) is rejected
        assert!(!matches_expected_token(
            "external.coding_workspace_task",
            Some(EXEC),
            Some(CONTROL),
            Some(CONTROL)
        ));
        // missing execution token fails closed
        assert!(!matches_expected_token(
            "external.coding_workspace_task",
            None,
            Some(CONTROL),
            Some(EXEC)
        ));
        // no bearer fails closed
        assert!(!matches_expected_token(
            "external.coding_workspace_task",
            Some(EXEC),
            Some(CONTROL),
            None
        ));
    }

    #[test]
    fn task_submit_authenticates_with_control_token_only() {
        assert!(matches_expected_token(
            "external.coding_task_submit",
            Some(EXEC),
            Some(CONTROL),
            Some(CONTROL)
        ));
        // execution token must NOT satisfy task_submit
        assert!(!matches_expected_token(
            "external.coding_task_submit",
            Some(EXEC),
            Some(CONTROL),
            Some(EXEC)
        ));
        assert!(!matches_expected_token(
            "external.coding_task_submit",
            Some(EXEC),
            None,
            Some(EXEC)
        ));
    }

    #[test]
    fn other_operations_do_not_authenticate() {
        assert!(matches_expected_token(
            "external.coding_workspace_exec",
            Some(EXEC),
            Some(CONTROL),
            None
        ));
    }

    /// workspace_task must never fake-succeed: an explicit non-opencode
    /// backend is rejected, and a missing backend resolves to opencode.
    #[test]
    fn workspace_task_never_uses_fake_backend() {
        use crate::config::CodingConfig;
        use serde_json::json;

        std::env::set_var(
            "CODING_CONFIG",
            r#"{"workspaces":{"scratch":{"root":"/tmp/ws","read":true,"write":true,"exec":true,"opencode":true,"network":true,"shell":false}}}"#,
        );
        let config = CodingConfig::from_env();
        std::env::remove_var("CODING_CONFIG");
        // explicit fake backend -> rejected
        let bad = dispatch(
            &config,
            "external.coding_workspace_task",
            &json!({"workspace_id": "scratch", "backend": "fake", "objective": "x"}),
        );
        assert_eq!(bad["error_code"], "backend_must_be_opencode", "got: {bad}");
        // missing backend -> real opencode task is queued
        let ok = dispatch(
            &config,
            "external.coding_workspace_task",
            &json!({"workspace_id": "scratch", "objective": "x"}),
        );
        assert_eq!(ok["result"]["backend"], "opencode", "got: {ok}");
    }

    #[test]
    fn execution_task_accepts_only_an_authorized_absolute_root() {
        use crate::config::CodingConfig;
        use serde_json::json;

        let root = std::env::temp_dir().join(format!(
            "coding-harness-path-task-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::env::set_var(
            "CODING_CONFIG",
            json!({
                "workspaces": {
                    "legacy-internal-id": {
                        "root": root,
                        "read": true,
                        "write": true,
                        "exec": true,
                        "opencode": true,
                        "network": true,
                        "shell": false
                    }
                }
            })
            .to_string(),
        );
        let config = CodingConfig::from_env();
        std::env::remove_var("CODING_CONFIG");

        assert_eq!(
            resolve_execution_task_root(&config, root.to_str()).unwrap(),
            root
        );
        assert_eq!(
            resolve_execution_task_root(&config, Some("/tmp")).unwrap_err(),
            "root_not_allowed"
        );
        assert_eq!(
            resolve_execution_task_root(&config, Some("relative")).unwrap_err(),
            "root_not_absolute"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
