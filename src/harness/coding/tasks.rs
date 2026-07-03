//! Task management for coding.task.submit and coding.task.status.
//!
//! Backend modes:
//! - "fake" (default for CI/testing): state machine without real subprocess.
//! - "zcode": spawns zcode subprocess (planned — binary not present).
//! - "opencode": spawns `opencode run` with DeepSeek-V4-Flash.
//!
//! State machine: queued → running → succeeded | failed | cancelled.
//! stdout/stderr are captured and bounded.

use serde_json::{json, Value};
use std::collections::HashMap;
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
    diff_summary: String,
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

/// Submit a new task. Returns task_id and queued status.
/// `backend` can be "fake", "zcode", or "opencode".
/// `workspace_root` is required for opencode backend (used as --dir).
/// `model` is the provider/model for opencode backend.
pub fn submit_task(
    workspace_id: &str,
    objective: &str,
    acceptance_criteria: &str,
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

    let backend_used = match backend {
        "zcode" => "zcode",
        "opencode" => "opencode",
        _ => "fake",
    };

    let task = Task {
        id: id.clone(),
        workspace_id: workspace_id.to_string(),
        workspace_root: workspace_root.unwrap_or(".").to_string(),
        objective: objective.to_string(),
        acceptance_criteria: acceptance_criteria.to_string(),
        backend: backend_used.to_string(),
        model: model.unwrap_or("").to_string(),
        status: TaskStatus::Queued,
        summary: String::new(),
        commit_sha: String::new(),
        diff_summary: String::new(),
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

    // Spawn async execution thread.
    let tid = id.clone();
    let ws = workspace_id.to_string();
    let wr = workspace_root.unwrap_or(".").to_string();
    let obj = objective.to_string();
    let acc = acceptance_criteria.to_string();
    let bk = backend_used.to_string();
    let mdl = model.unwrap_or("").to_string();
    std::thread::spawn(move || {
        execute_task(&tid, &ws, &wr, &obj, &acc, &bk, &mdl);
    });

    ok(json!({"task_id": id, "status": "queued", "backend": backend_used, "created_at": now}))
}

/// Run the task in background thread.
fn execute_task(
    task_id: &str,
    _workspace_id: &str,
    workspace_root: &str,
    objective: &str,
    acceptance_criteria: &str,
    backend: &str,
    model: &str,
) {
    // Transition to Running.
    {
        let store = tasks().lock().unwrap();
        if let Some(t) = store.get(task_id) {
            if t.status != TaskStatus::Queued {
                return; // was cancelled before we started
            }
        }
    }
    {
        let mut store = tasks().lock().unwrap();
        if let Some(t) = store.get_mut(task_id) {
            t.status = TaskStatus::Running;
            t.updated_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
    }

    let result = match backend {
        "opencode" => run_opencode(workspace_root, objective, acceptance_criteria, model),
        "zcode" => run_zcode(objective, acceptance_criteria),
        _ => run_fake(objective, acceptance_criteria),
    };

    let mut store = tasks().lock().unwrap();
    if let Some(t) = store.get_mut(task_id) {
        match t.status {
            TaskStatus::Cancelled => {
                return;
            }
            TaskStatus::Running => {
                match result {
                    Ok(out) => {
                        t.status = TaskStatus::Succeeded;
                        t.exit_code = out.exit_code;
                        t.summary = out.summary;
                        t.commit_sha = out.commit_sha;
                        t.diff_summary = out.diff_summary;
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

pub(crate) struct TaskOutput {
    pub(crate) summary: String,
    pub(crate) commit_sha: String,
    pub(crate) diff_summary: String,
    pub(crate) test_result: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: i32,
    pub(crate) timed_out: bool,
}

/// Fake backend: simulates a successful task execution for CI/testing.
fn run_fake(objective: &str, acceptance_criteria: &str) -> Result<TaskOutput, String> {
    std::thread::sleep(Duration::from_millis(10));
    Ok(TaskOutput {
        summary: format!(
            "fake: completed objective '{}'",
            truncate_str(objective, 80)
        ),
        commit_sha: "fake_sha_0000000000000000000000000000000000000000".into(),
        diff_summary: "fake: all files processed".into(),
        test_result: format!(
            "acceptance: '{}' passed",
            truncate_str(acceptance_criteria, 80)
        ),
        stdout: "fake: task completed successfully\n".into(),
        stderr: String::new(),
        exit_code: 0,
        timed_out: false,
    })
}

/// ZCode backend (planned): spawns a zcode subprocess.
fn run_zcode(objective: &str, acceptance_criteria: &str) -> Result<TaskOutput, String> {
    let zcode_path = find_zcode().map_err(|e| format!("zcode_not_found: {e}"))?;
    let mut cmd = std::process::Command::new(&zcode_path);
    cmd.arg("--objective")
        .arg(objective)
        .arg("--acceptance")
        .arg(acceptance_criteria);
    cmd.env_clear();
    if let Some(v) = std::env::var_os("PATH") {
        cmd.env("PATH", v);
    }
    if let Some(v) = std::env::var_os("HOME") {
        cmd.env("HOME", v);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = cmd
        .output()
        .map_err(|e| format!("zcode_spawn_failed: {e}"))?;
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
    if exit_code == 0 {
        Ok(TaskOutput {
            summary: format!(
                "zcode: completed '{}' (exit={})",
                truncate_str(objective, 80),
                exit_code
            ),
            commit_sha: extract_sha_from_output(&stdout_str),
            diff_summary: "zcode: see output".into(),
            test_result: format!(
                "zcode exit={} acceptance='{}'",
                exit_code,
                truncate_str(acceptance_criteria, 80)
            ),
            stdout: stdout_str,
            stderr: stderr_str,
            exit_code,
            timed_out: false,
        })
    } else {
        Err(format!(
            "zcode_exit_{}: {}",
            exit_code,
            truncate_str(&stdout_str.lines().last().unwrap_or(&stdout_str), 200)
        ))
    }
}

fn find_zcode() -> Result<String, String> {
    for path in &["zcode", "/usr/local/bin/zcode", "/opt/homebrew/bin/zcode"] {
        if std::process::Command::new(path)
            .arg("--version")
            .output()
            .is_ok()
        {
            return Ok(path.to_string());
        }
    }
    Err("zcode binary not found in PATH or common locations".into())
}

fn run_opencode(
    workspace_root: &str,
    objective: &str,
    acceptance_criteria: &str,
    model: &str,
) -> Result<TaskOutput, String> {
    crate::harness::coding::opencode_backend::run_opencode(
        workspace_root,
        objective,
        acceptance_criteria,
        model,
    )
}

fn extract_sha_from_output(output: &str) -> String {
    for line in output.lines() {
        if line.starts_with("commit:") || line.starts_with("sha:") {
            return line
                .split_once(':')
                .map(|(_, v)| v.trim())
                .unwrap_or("")
                .to_string();
        }
    }
    String::new()
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut r: String = s.chars().take(max).collect();
        r.truncate(r.len().saturating_sub(3));
        r.push_str("...");
        r
    }
}

// ── Status ──

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
                "diff_summary": t.diff_summary,
                "test_result": t.test_result,
                "exit_code": t.exit_code,
                "timed_out": t.timed_out,
                "stdout_truncated": t.stdout_bounded,
                "stderr_truncated": t.stderr_bounded,
            });
            if let TaskStatus::Failed(ref reason) = t.status {
                resp["error"] = json!(reason);
                resp["failure_reason"] = json!(reason);
            }
            if t.status == TaskStatus::Cancelled {
                resp["error"] = json!("cancelled");
            }
            ok(resp)
        }
        None => err("task_not_found"),
    }
}

// ── Cancel ──

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

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
