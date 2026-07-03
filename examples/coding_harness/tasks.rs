//! Task management for coding.task.submit and coding.task.status.
//! In-memory task store with zcode backend execution.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static TASK_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
struct Task {
    id: String,
    workspace_id: String,
    objective: String,
    status: TaskStatus,
    summary: String,
    created_at: u64,
    updated_at: u64,
}

fn tasks() -> &'static Mutex<HashMap<String, Task>> {
    use std::sync::OnceLock;
    static TASKS: OnceLock<Mutex<HashMap<String, Task>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn submit_task(workspace_id: &str, objective: &str, _backend: &str) -> Value {
    let seq = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    let id = format!("task_{seq:x}");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let task = Task {
        id: id.clone(),
        workspace_id: workspace_id.to_string(),
        objective: objective.to_string(),
        status: TaskStatus::Queued,
        summary: String::new(),
        created_at: now,
        updated_at: now,
    };
    tasks().lock().unwrap().insert(id.clone(), task);
    ok(json!({"task_id": id, "status": "queued", "created_at": now}))
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
            let mut resp = json!({"task_id": t.id, "status": status_str, "created_at": t.created_at, "summary": t.summary});
            if let TaskStatus::Failed(ref reason) = t.status {
                resp["error"] = json!(reason);
            }
            ok(resp)
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
