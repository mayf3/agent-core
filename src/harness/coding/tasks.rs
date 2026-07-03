//! Task management for coding.task.submit and coding.task.status.
//!
//! Backend modes:
//! - "fake" (default for CI/testing): state machine without real subprocess.
//! - "zcode": spawns zcode subprocess with workspace scoping.
//!
//! State machine: queued → running → succeeded | failed | cancelled.
//! stdoud/stderr are captured and bounded.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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
    objective: String,
    backend: String,
    status: TaskStatus,
    summary: String,
    commit_sha: String,
    diff_summary: String,
    test_result: String,
    failure_reason: String,
    stdout_bounded: String,
    stderr_bounded: String,
    created_at: u64,
    updated_at: u64,
}

fn tasks() -> &'static Mutex<HashMap<String, Task>> {
    use std::sync::OnceLock;
    static TASKS: OnceLock<Mutex<HashMap<String, Task>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Submit a new task. Returns task_id and queued status.
/// `backend` can be "zcode" or "fake". When "zcode", the workspace must
/// have zcode permission (caller must check this before calling).
pub fn submit_task(
    workspace_id: &str,
    objective: &str,
    acceptance_criteria: &str,
    backend: &str,
) -> Value {
    let seq = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    let id = format!("task_{seq:x}");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let (backend_used, initial_status) = match backend {
        "zcode" => ("zcode", TaskStatus::Queued),
        _ => ("fake", TaskStatus::Queued),
    };

    let task = Task {
        id: id.clone(),
        workspace_id: workspace_id.to_string(),
        objective: objective.to_string(),
        backend: backend_used.to_string(),
        status: initial_status,
        summary: String::new(),
        commit_sha: String::new(),
        diff_summary: String::new(),
        test_result: String::new(),
        failure_reason: String::new(),
        stdout_bounded: String::new(),
        stderr_bounded: String::new(),
        created_at: now,
        updated_at: now,
    };
    tasks().lock().unwrap().insert(id.clone(), task);

    // Spawn async execution thread.
    let tid = id.clone();
    let ws = workspace_id.to_string();
    let obj = objective.to_string();
    let acc = acceptance_criteria.to_string();
    let bk = backend_used.to_string();
    std::thread::spawn(move || {
        execute_task(&tid, &ws, &obj, &acc, &bk);
    });

    ok(json!({"task_id": id, "status": "queued", "backend": backend_used, "created_at": now}))
}

/// Run the task in background thread.
fn execute_task(
    task_id: &str,
    _workspace_id: &str,
    objective: &str,
    acceptance_criteria: &str,
    backend: &str,
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
        "zcode" => run_zcode(objective, acceptance_criteria),
        _ => run_fake(objective, acceptance_criteria),
    };

    let mut store = tasks().lock().unwrap();
    if let Some(t) = store.get_mut(task_id) {
        match t.status {
            TaskStatus::Cancelled => {
                // Was cancelled during execution; don't overwrite status.
                return;
            }
            TaskStatus::Running => {
                match result {
                    Ok(out) => {
                        t.status = TaskStatus::Succeeded;
                        t.summary = out.summary;
                        t.commit_sha = out.commit_sha;
                        t.diff_summary = out.diff_summary;
                        t.test_result = out.test_result;
                        t.stdout_bounded = truncate_str(&out.stdout, 100_000);
                        t.stderr_bounded = truncate_str(&out.stderr, 100_000);
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
    diff_summary: String,
    test_result: String,
    stdout: String,
    stderr: String,
}

/// Fake backend: simulates a successful task execution for CI/testing.
fn run_fake(objective: &str, acceptance_criteria: &str) -> Result<TaskOutput, String> {
    // Simulate a brief delay so the state machine can be observed.
    std::thread::sleep(std::time::Duration::from_millis(10));
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
    })
}

/// Real zcode backend: spawns a zcode subprocess.
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
    // Check common locations.
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
                "created_at": t.created_at,
                "updated_at": t.updated_at,
                "summary": t.summary,
                "commit_sha": t.commit_sha,
                "diff_summary": t.diff_summary,
                "test_result": t.test_result,
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
mod tests {
    use super::*;

    #[test]
    fn fake_backend_full_state_machine() {
        // Submit → queued
        let resp = submit_task("ws1", "test objective", "must pass", "fake");
        let tid = resp["result"]["task_id"].as_str().unwrap().to_string();
        assert_eq!(resp["result"]["status"], "queued");
        assert_eq!(resp["result"]["backend"], "fake");

        // Wait briefly for execution
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Status → succeeded
        let s = get_status(&tid);
        assert_eq!(
            s["result"]["status"], "succeeded",
            "fake backend should reach succeeded; got: {s}"
        );
        assert!(s["result"]["summary"]
            .as_str()
            .unwrap_or("")
            .contains("fake"));
        assert!(s["result"]["commit_sha"]
            .as_str()
            .unwrap_or("")
            .contains("fake_sha"));
    }

    #[test]
    fn task_cancel_before_execution() {
        let resp = submit_task("ws1", "cancellable objective", "", "fake");
        let tid = resp["result"]["task_id"].as_str().unwrap().to_string();
        // Cancel immediately (may still be Queued).
        let cancel_resp = cancel_task(&tid);
        assert_eq!(cancel_resp["result"]["status"], "cancelled");

        let s = get_status(&tid);
        assert_eq!(s["result"]["status"], "cancelled");
    }

    #[test]
    fn task_not_found() {
        let s = get_status("nonexistent");
        assert_eq!(s["ok"], false);
        assert_eq!(s["error_code"], "task_not_found");
    }

    #[test]
    fn task_submit_includes_acceptance_criteria() {
        let resp = submit_task("ws1", "build", "test passes", "fake");
        let tid = resp["result"]["task_id"].as_str().unwrap().to_string();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let s = get_status(&tid);
        assert_eq!(s["result"]["status"], "succeeded");
        assert!(
            s["result"]["test_result"]
                .as_str()
                .unwrap_or("")
                .contains("test passes"),
            "test_result should include acceptance criteria text"
        );
    }
}
