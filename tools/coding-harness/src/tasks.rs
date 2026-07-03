use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static TASK_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Task {
    id: String,
    workspace_id: String,
    workspace_root: String,
    objective: String,
    acceptance_criteria: String,
    backend: String,
    model: String,
    status: TaskStatus,
    summary: String,
    commit_sha: String,
    changed_files: String,
    diff_summary: String,
    test_command: String,
    test_result: String,
    failure_reason: String,
    stdout_bounded: String,
    stderr_bounded: String,
    exit_code: i32,
    timed_out: bool,
    created_at: u64,
    updated_at: u64,
}

fn tasks() -> &'static Mutex<HashMap<String, Task>> {
    use std::sync::OnceLock;
    static TASKS: OnceLock<Mutex<HashMap<String, Task>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Normalize acceptance_criteria: accepts string or array of strings.
pub fn normalize_acceptance(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let items: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            items.join("\n")
        }
        _ => String::new(),
    }
}

pub fn submit_task(
    workspace_id: &str,
    objective: &str,
    acceptance_criteria: &Value,
    backend: &str,
    workspace_root: Option<&str>,
    model: Option<&str>,
) -> Value {
    let seq = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    let id = format!("task_{seq:x}");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let acc = normalize_acceptance(acceptance_criteria);

    let (backend_used, initial_status) = match backend {
        "opencode" => ("opencode", TaskStatus::Queued),
        "fake" => ("fake", TaskStatus::Queued),
        other => return err(&format!("unsupported_backend: {other}")),
    };

    let task = Task {
        id: id.clone(),
        workspace_id: workspace_id.to_string(),
        workspace_root: workspace_root.unwrap_or(".").to_string(),
        objective: objective.to_string(),
        acceptance_criteria: acc,
        backend: backend_used.to_string(),
        model: model.unwrap_or("").to_string(),
        status: initial_status,
        summary: String::new(),
        commit_sha: String::new(),
        changed_files: String::new(),
        diff_summary: String::new(),
        test_command: String::new(),
        test_result: String::new(),
        failure_reason: String::new(),
        stdout_bounded: String::new(),
        stderr_bounded: String::new(),
        exit_code: -1,
        timed_out: false,
        created_at: now,
        updated_at: now,
    };
    tasks().lock().unwrap().insert(id.clone(), task);

    let tid = id.clone();
    let ws = workspace_id.to_string();
    let wr = workspace_root.unwrap_or(".").to_string();
    let obj = objective.to_string();
    let bk = backend_used.to_string();
    let mdl = model.unwrap_or("").to_string();
    std::thread::spawn(move || execute_task(&tid, &ws, &wr, &obj, &bk, &mdl));

    ok(json!({"task_id": id, "status": "queued", "backend": backend_used, "created_at": now}))
}

fn execute_task(
    task_id: &str,
    _workspace_id: &str,
    workspace_root: &str,
    objective: &str,
    backend: &str,
    model: &str,
) {
    // Transition to Running.
    {
        let mut store = tasks().lock().unwrap();
        if let Some(t) = store.get_mut(task_id) {
            if t.status != TaskStatus::Queued {
                return;
            }
            t.status = TaskStatus::Running;
            t.updated_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
    }

    let result = match backend {
        "opencode" => run_opencode(workspace_root, objective, model),
        _ => run_fake(objective),
    };

    let mut store = tasks().lock().unwrap();
    if let Some(t) = store.get_mut(task_id) {
        match t.status {
            TaskStatus::Cancelled => return,
            TaskStatus::Running => {
                match result {
                    Ok(out) => {
                        t.status = TaskStatus::Succeeded;
                        t.exit_code = out.exit_code;
                        t.summary = out.summary;
                        t.commit_sha = out.commit_sha;
                        t.changed_files = out.changed_files;
                        t.diff_summary = out.diff_summary;
                        t.test_command = out.test_command;
                        t.test_result = out.test_result;
                        t.stdout_bounded = truncate_str(&out.stdout, 100_000);
                        t.stderr_bounded = truncate_str(&out.stderr, 100_000);
                        t.timed_out = out.timed_out;
                    }
                    Err(e) => {
                        t.status = TaskStatus::Failed(e.clone());
                        t.failure_reason = e;
                    }
                }
                t.updated_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            }
            _ => {}
        }
    }
}

struct TaskOutput {
    summary: String,
    commit_sha: String,
    changed_files: String,
    diff_summary: String,
    test_command: String,
    test_result: String,
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
}

fn run_fake(objective: &str) -> Result<TaskOutput, String> {
    std::thread::sleep(Duration::from_millis(10));
    Ok(TaskOutput {
        summary: format!("fake: completed '{}'", truncate_str(objective, 80)),
        commit_sha: String::new(),
        changed_files: String::new(),
        diff_summary: "fake: no real changes".into(),
        test_command: "cargo test".into(),
        test_result: "not_reported".into(),
        stdout: "fake: task completed\n".into(),
        stderr: String::new(),
        exit_code: 0,
        timed_out: false,
    })
}

fn run_opencode(workspace_root: &str, objective: &str, model: &str) -> Result<TaskOutput, String> {
    let opencode_path = find_opencode().map_err(|e| format!("opencode_not_found: {e}"))?;
    let resolved_model = if model.is_empty() {
        "deepseek/deepseek-v4-flash"
    } else {
        model
    };

    let prompt = build_prompt(objective);

    let mut cmd = std::process::Command::new(&opencode_path);
    cmd.arg("run")
        .arg("--model")
        .arg(resolved_model)
        .arg("--format")
        .arg("json")
        .arg("--dir")
        .arg(workspace_root)
        .arg("--dangerously-skip-permissions")
        .arg(&prompt);

    cmd.env_clear();
    for var in &["PATH", "HOME", "TMPDIR", "DEEPSEEK_API_KEY"] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let deadline = Duration::from_secs(600);
    let start = std::time::Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("opencode_spawn_failed: {e}"))?;

    let (exit_code, stdout_str, stderr_str, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                let _ = child.stdout.take().map(|mut s| s.read_to_end(&mut stdout));
                let _ = child.stderr.take().map(|mut s| s.read_to_end(&mut stderr));
                break (
                    status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&stdout).to_string(),
                    String::from_utf8_lossy(&stderr).to_string(),
                    false,
                );
            }
            Ok(None) => {}
            Err(_) => break (-1, String::new(), String::new(), false),
        }
        if start.elapsed() >= deadline {
            #[cfg(unix)]
            unsafe {
                let _ = libc::kill(child.id() as libc::pid_t, libc::SIGKILL);
            }
            #[cfg(not(unix))]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(&["/F", "/T", "/PID", &child.id().to_string()])
                    .output();
            }
            let _ = child.wait();
            break (-1, String::new(), "timed_out".to_string(), true);
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    if exit_code == 0 {
        let (commit_sha, changed_files, diff_summary, test_command, test_result, summary) =
            parse_opencode_output(&stdout_str, objective);
        Ok(TaskOutput {
            summary,
            commit_sha,
            changed_files,
            diff_summary,
            test_command,
            test_result,
            stdout: stdout_str,
            stderr: stderr_str,
            exit_code,
            timed_out,
        })
    } else {
        Err(format!(
            "opencode_exit_{}: {}",
            exit_code,
            truncate_str(&stderr_str.lines().last().unwrap_or(&stderr_str), 300)
        ))
    }
}

fn find_opencode() -> Result<String, String> {
    if std::process::Command::new("opencode")
        .arg("--version")
        .output()
        .is_ok()
    {
        Ok("opencode".to_string())
    } else {
        Err("opencode binary not found in PATH".into())
    }
}

fn build_prompt(objective: &str) -> String {
    format!(
        "Objective\n{objective}\n\n\
         Workspace boundary\n\
         - You may ONLY modify files within the current workspace directory.\n\
         - You MUST NOT access files outside the workspace.\n\
         - You MUST NOT read .env, tokens, keys, or production secrets.\n\
         - You MUST NOT push, merge, or deploy code.\n\n\
         Testing requirements\n\
         - After making changes, run the project's test suite.\n\
         - All existing tests must continue to pass.\n\n\
         Completion reporting\n\
         - Report which files were changed and why.\n\
         - Report test results and any failures.\n\
         - Keep output concise."
    )
}

fn parse_opencode_output(
    stdout: &str,
    objective: &str,
) -> (String, String, String, String, String, String) {
    let mut commit_sha = String::new();
    let mut changed_files = String::new();
    let mut diff_summary = String::new();
    let mut test_command = String::new();
    let mut test_result = "not_reported".to_string();
    let mut summary = format!("opencode: completed '{}'", truncate_str(objective, 80));

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
            if let Some(et) = event.get("type").and_then(Value::as_str) {
                match et {
                    "completion" | "result" | "done" => {
                        if let Some(c) = event.get("content").and_then(Value::as_str) {
                            summary = truncate_str(c, 200).to_string();
                        }
                    }
                    "file_change" | "edit" | "write" => {
                        if let Some(p) = event.get("path").and_then(Value::as_str) {
                            if !changed_files.is_empty() {
                                changed_files.push_str(", ");
                            }
                            changed_files.push_str(p);
                        }
                    }
                    "diff" => {
                        if let Some(d) = event.get("diff").and_then(Value::as_str) {
                            diff_summary = truncate_str(d, 500).to_string();
                        }
                    }
                    "test" | "test_result" => {
                        if let Some(s) = event.get("status").and_then(Value::as_str) {
                            test_result = format!("test: {}", s);
                        }
                        if let Some(o) = event.get("output").and_then(Value::as_str) {
                            test_result = truncate_str(o, 200).to_string();
                        }
                    }
                    "bash" | "tool_use" => {
                        if let Some(cmd_name) = event.pointer("/tool").and_then(Value::as_str) {
                            if cmd_name == "bash" {
                                if let Some(input) = event
                                    .pointer("/state/input/command")
                                    .and_then(Value::as_str)
                                {
                                    test_command = truncate_str(input, 200).to_string();
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if commit_sha.is_empty() {
                if let Some(sha) = event.get("commit_sha").and_then(Value::as_str) {
                    commit_sha = sha.to_string();
                }
            }
        }
    }
    if changed_files.is_empty() {
        changed_files = "unknown".to_string();
    }
    (
        commit_sha,
        changed_files,
        diff_summary,
        test_command,
        test_result,
        summary,
    )
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut r: String = s.chars().take(max).collect();
        r.truncate(r.len().saturating_sub(3));
        r.push_str("...");
        r
    }
}

pub fn get_status(task_id: &str) -> Value {
    let store = tasks().lock().unwrap();
    match store.get(task_id) {
        Some(t) => {
            let status_str = match &t.status {
                TaskStatus::Queued => "queued",
                TaskStatus::Running => "running",
                TaskStatus::Succeeded => "succeeded",
                TaskStatus::Failed(_) => "failed",
                TaskStatus::Cancelled => "cancelled",
            };
            let mut resp = json!({
                "task_id": t.id,
                "status": status_str,
                "backend": t.backend,
                "model": t.model,
                "created_at": t.created_at,
                "updated_at": t.updated_at,
                "summary": t.summary,
                "commit_sha": t.commit_sha,
                "changed_files": t.changed_files,
                "diff_summary": t.diff_summary,
                "test_command": t.test_command,
                "test_result": t.test_result,
                "exit_code": t.exit_code,
                "timed_out": t.timed_out,
                "stdout_truncated": t.stdout_bounded,
                "stderr_truncated": t.stderr_bounded,
            });
            if let TaskStatus::Failed(ref reason) = t.status {
                resp["failure_reason"] = json!(reason);
            }
            ok(resp)
        }
        None => err("task_not_found"),
    }
}

pub fn cancel_task(task_id: &str) -> Value {
    let mut store = tasks().lock().unwrap();
    match store.get_mut(task_id) {
        Some(t) => {
            if t.status == TaskStatus::Queued || t.status == TaskStatus::Running {
                t.status = TaskStatus::Cancelled;
                t.updated_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                ok(json!({"task_id": task_id, "status": "cancelled"}))
            } else {
                err("task_not_cancellable")
            }
        }
        None => err("task_not_found"),
    }
}

fn ok(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
fn err(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}
