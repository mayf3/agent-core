//! Task management for `coding.task.submit` and `coding.task.status`.
//!
//! Supported backends:
//! - `"fake"`: simulated state machine (testing/CI).
//! - `"opencode"`: real OpenCode + DeepSeek-V4-Flash execution.

#[path = "opencode_backend.rs"]
mod opencode_backend;

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

pub(crate) struct TaskOutput {
    pub(crate) summary: String,
    pub(crate) commit_sha: String,
    pub(crate) changed_files: String,
    pub(crate) diff_summary: String,
    pub(crate) test_command: String,
    pub(crate) test_result: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: i32,
    pub(crate) timed_out: bool,
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
        test_result: "not_reported".into(),
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

    // Register cancel token for opencode backend.
    let cancel_flag = if bk == "opencode" {
        Some(opencode_backend::register_cancel(&tid))
    } else {
        None
    };

    std::thread::spawn(move || execute_task(&tid, &ws, &wr, &obj, &bk, &mdl, cancel_flag));

    ok(json!({"task_id": id, "status": "queued", "backend": backend_used, "created_at": now}))
}

fn execute_task(
    task_id: &str,
    _workspace_id: &str,
    workspace_root: &str,
    objective: &str,
    backend: &str,
    model: &str,
    cancel_flag: Option<Arc<AtomicBool>>,
) {
    // If queued task was cancelled before starting, don't run.
    if let Some(ref flag) = cancel_flag {
        if flag.load(Ordering::SeqCst) {
            return;
        }
    }

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
        "opencode" => {
            opencode_backend::run_opencode(task_id, workspace_root, objective, model, cancel_flag)
        }
        _ => run_fake(objective),
    };

    // Clean up cancel token after execution.
    opencode_backend::cleanup_cancel(task_id);

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
                "task_id": t.id, "status": status_str, "backend": t.backend, "model": t.model,
                "created_at": t.created_at, "updated_at": t.updated_at,
                "summary": t.summary, "commit_sha": t.commit_sha,
                "changed_files": t.changed_files, "diff_summary": t.diff_summary,
                "test_command": t.test_command, "test_result": t.test_result,
                "exit_code": t.exit_code, "timed_out": t.timed_out,
                "stdout_truncated": t.stdout_bounded, "stderr_truncated": t.stderr_bounded,
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
    // First, stop any running process via the cancel token.
    let killed = opencode_backend::cancel_task(task_id);

    let mut store = tasks().lock().unwrap();
    match store.get_mut(task_id) {
        Some(t) => {
            if t.status == TaskStatus::Queued || t.status == TaskStatus::Running {
                t.status = TaskStatus::Cancelled;
                t.updated_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                ok(json!({"task_id": task_id, "status": "cancelled", "process_killed": killed}))
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
