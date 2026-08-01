//! OpenCode backend for segmented development jobs.
//!
//! One segment = one bounded `opencode run` invocation. The Harness enforces
//! the frozen segment budget live: wall-clock deadline, model-round and
//! tool-call counts (parsed from the JSON event stream), per-tool silence
//! timeout, and the job cancel flag. When a segment is killed by budget, the
//! model's trailing checkpoint JSON is extracted from the output and becomes
//! the continuation context for the next segment.
//!
//! Config is passed via `OPENCODE_CONFIG_CONTENT` env var with proper
//! `"allow"/"deny"` permission semantics. No `.opencode.json` written to
//! workspace. Process lifecycle uses process-group cleanup and concurrent
//! pipe draining.

use crate::jobs::{
    truncate_str, Checkpoint, ExecutionSegmentBudget, Job, SegmentEnd, SegmentOutcome,
};
use serde_json::Value;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_OUTPUT: usize = 100_000;

/// Live counters observed from the `opencode run --format json` event
/// stream. The Harness (not the model) decides when a budget limit is hit.
struct EventMonitor {
    rounds: AtomicU64,
    tools: AtomicU64,
    last_event_at: AtomicU64, // unix nanos; 0 = no event yet
    should_kill: AtomicBool,
    kill_reason: Mutex<String>,
}

impl EventMonitor {
    fn new() -> Self {
        Self {
            rounds: AtomicU64::new(0),
            tools: AtomicU64::new(0),
            last_event_at: AtomicU64::new(0),
            should_kill: AtomicBool::new(false),
            kill_reason: Mutex::new(String::new()),
        }
    }

    fn observe_event(&self, event: &Value) {
        self.last_event_at.store(now_nanos(), Ordering::Relaxed);
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            // One assistant message = one model round.
            "message" => {
                if event.get("role").and_then(Value::as_str) == Some("assistant") {
                    self.rounds.fetch_add(1, Ordering::Relaxed);
                }
            }
            // Tool executions / subagent work = tool calls.
            "tool" | "agent" | "shell" => {
                self.tools.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                if event.get("tool_use").is_some() {
                    self.tools.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn request_kill(&self, reason: &str) {
        if self
            .should_kill
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            *self.kill_reason.lock().unwrap() = reason.to_string();
        }
    }
}

/// Run one bounded segment with the OpenCode backend.
pub(super) fn run_segment(
    job: &Job,
    budget: &ExecutionSegmentBudget,
    checkpoint: Option<&Checkpoint>,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> SegmentOutcome {
    let opencode_path = match find_opencode() {
        Ok(p) => p,
        Err(e) => {
            return SegmentOutcome {
                end: SegmentEnd::Failed(format!("opencode_not_found: {e}")),
                model_rounds_used: 0,
                tool_calls_used: 0,
                wall_time_ms: 0,
                checkpoint: None,
            }
        }
    };
    let resolved_model = if job.model.is_empty() {
        "deepseek/deepseek-v4-flash"
    } else {
        &job.model
    };
    let prompt = build_prompt(job, checkpoint);
    let ws_root = job.workspace_root.clone();

    let mut cmd = std::process::Command::new(&opencode_path);
    cmd.arg("run")
        .arg("--model")
        .arg(resolved_model)
        .arg("--format")
        .arg("json")
        .arg("--dir")
        .arg(&ws_root)
        .arg(&prompt);

    cmd.env_clear();
    for var in &["PATH", "HOME", "TMPDIR", "DEEPSEEK_API_KEY"] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    // Pass permission config via env var, not .opencode.json file.
    cmd.env("OPENCODE_CONFIG_CONTENT", build_config_json());

    // Create process group for tree-kill.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return SegmentOutcome {
                end: SegmentEnd::Failed(format!("opencode_spawn_failed: {e}")),
                model_rounds_used: 0,
                tool_calls_used: 0,
                wall_time_ms: 0,
                checkpoint: None,
            }
        }
    };
    let pid = child.id();

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    let monitor = Arc::new(EventMonitor::new());

    if let Some(pipe) = stdout_pipe {
        let b = Arc::clone(&out_buf);
        let d = Arc::clone(&done);
        let m = Arc::clone(&monitor);
        std::thread::spawn(move || drain_stdout(pipe, b, d, MAX_OUTPUT, m));
    }
    if let Some(pipe) = stderr_pipe {
        let b = Arc::clone(&err_buf);
        let d = Arc::clone(&done);
        std::thread::spawn(move || drain_pipe(pipe, b, d, MAX_OUTPUT));
    }

    let start = std::time::Instant::now();
    let mut timed_out = false;
    let mut cancelled = false;
    let mut kill_reason = String::new();

    loop {
        if let Some(ref flag) = cancel_flag {
            if flag.load(Ordering::SeqCst) {
                cancelled = true;
                done.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                let _ = child.wait();
                break;
            }
        }
        if monitor.should_kill.load(Ordering::SeqCst) {
            kill_reason = monitor.kill_reason.lock().unwrap().clone();
            timed_out = true;
            done.store(true, Ordering::SeqCst);
            kill_process_group(pid);
            let _ = child.wait();
            break;
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() >= Duration::from_millis(budget.max_wall_time_ms) {
            kill_reason = "max_wall_time_ms".into();
            timed_out = true;
            done.store(true, Ordering::SeqCst);
            kill_process_group(pid);
            let _ = child.wait();
            break;
        }
        if budget.single_tool_timeout_ms > 0 {
            let last = monitor.last_event_at.load(Ordering::Relaxed);
            if last != 0
                && now_nanos().saturating_sub(last)
                    > budget.single_tool_timeout_ms * 1_000_000
            {
                kill_reason = "single_tool_timeout".into();
                timed_out = true;
                done.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                let _ = child.wait();
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done.store(true, Ordering::SeqCst);
    let exit_code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    let wall_time_ms = start.elapsed().as_millis() as u64;

    if cancelled {
        return SegmentOutcome {
            end: SegmentEnd::Cancelled,
            model_rounds_used: monitor.rounds.load(Ordering::Relaxed),
            tool_calls_used: monitor.tools.load(Ordering::Relaxed),
            wall_time_ms,
            checkpoint: None,
        };
    }

    let stdout_all = out_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stderr_all = err_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stdout_str = String::from_utf8_lossy(&stdout_all).to_string();
    let stderr_str = String::from_utf8_lossy(&stderr_all).to_string();

    let parsed_checkpoint = parse_checkpoint(&stdout_str, &job.workspace_root);
    let rounds_used = monitor.rounds.load(Ordering::Relaxed);
    let tools_used = monitor.tools.load(Ordering::Relaxed);

    if exit_code != 0 {
        return SegmentOutcome {
            end: SegmentEnd::Failed(format!(
                "opencode_exit_{}: {}",
                exit_code,
                truncate_str(stderr_str.lines().last().unwrap_or(&stderr_str), 300)
            )),
            model_rounds_used: rounds_used,
            tool_calls_used: tools_used,
            wall_time_ms,
            checkpoint: parsed_checkpoint,
        };
    }

    let result = build_result(&stdout_str, &job.objective, exit_code, timed_out);
    let work_remaining = parsed_checkpoint
        .as_ref()
        .map(|cp| !cp.remaining_steps.is_empty())
        .unwrap_or(true);
    let end = if work_remaining {
        let reason = if timed_out {
            kill_reason
        } else {
            "model_reports_work_remaining".into()
        };
        SegmentEnd::Exhausted(reason)
    } else {
        SegmentEnd::Completed(result)
    };
    SegmentOutcome {
        end,
        model_rounds_used: rounds_used,
        tool_calls_used: tools_used,
        wall_time_ms,
        checkpoint: parsed_checkpoint,
    }
}

/// Drain stdout with live JSON event observation and a byte limit.
fn drain_stdout(
    mut pipe: impl Read,
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
    max: usize,
    monitor: Arc<EventMonitor>,
) {
    let mut local = Vec::new();
    let mut line_buf = Vec::new();
    let mut tmp = [0u8; 65536];
    loop {
        if done.load(Ordering::SeqCst) {
            let mut rest = Vec::new();
            let _ = pipe.read_to_end(&mut rest);
            if !rest.is_empty() && local.len() < max {
                let remaining = max.saturating_sub(local.len());
                local.extend_from_slice(&rest[..rest.len().min(remaining)]);
            }
            break;
        }
        match pipe.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                for &byte in &tmp[..n] {
                    if byte == b'\n' {
                        let line = std::mem::take(&mut line_buf);
                        if let Ok(event) = serde_json::from_slice::<Value>(&line) {
                            monitor.observe_event(&event);
                        }
                    } else {
                        if line_buf.len() < 4096 {
                            line_buf.push(byte);
                        }
                    }
                }
                if local.len() < max {
                    local.extend_from_slice(&tmp[..n.min(max.saturating_sub(local.len()))]);
                }
            }
            Err(_) => break,
        }
    }
    buf.lock().unwrap().extend_from_slice(&local);
}

/// Drain a plain pipe with a byte limit (stderr).
fn drain_pipe(mut pipe: impl Read, buf: Arc<Mutex<Vec<u8>>>, done: Arc<AtomicBool>, max: usize) {
    let mut local = Vec::new();
    let mut tmp = [0u8; 65536];
    loop {
        if done.load(Ordering::SeqCst) {
            let mut rest = Vec::new();
            let _ = pipe.read_to_end(&mut rest);
            if !rest.is_empty() && local.len() < max {
                let remaining = max.saturating_sub(local.len());
                local.extend_from_slice(&rest[..rest.len().min(remaining)]);
            }
            break;
        }
        match pipe.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                if local.len() < max {
                    local.extend_from_slice(&tmp[..n.min(max.saturating_sub(local.len()))]);
                }
            }
            Err(_) => break,
        }
    }
    buf.lock().unwrap().extend_from_slice(&local);
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    unsafe {
        let pgid = pid as libc::pid_t;
        let _ = libc::killpg(pgid, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(500));
        let _ = libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(&["/F", "/T", "/PID", &pid.to_string()])
        .output();
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

/// Build the per-segment prompt: objective + acceptance + previous
/// checkpoint (continuation contract) + boundaries + checkpoint reporting.
fn build_prompt(job: &Job, checkpoint: Option<&Checkpoint>) -> String {
    let criteria_section = if job.acceptance_criteria.is_empty() {
        String::new()
    } else {
        format!(
            "Acceptance criteria\n\
             {}\n\n",
            job.acceptance_criteria
        )
    };
    let checkpoint_section = match checkpoint {
        Some(cp) => format!(
            "Previous checkpoint (from the last execution segment)\n\
             - Findings: {}\n\
             - Completed steps: {}\n\
             - Remaining steps: {}\n\
             - Last test result: {}\n\
             - Blocker: {}\n\
             - Next action: {}\n\
             Continue from this checkpoint. Do NOT redo completed work.\n\n",
            cp.findings,
            cp.completed_steps.join("; "),
            cp.remaining_steps.join("; "),
            cp.last_test_result,
            cp.blocker,
            cp.next_action,
        ),
        None => String::new(),
    };
    format!(
        "Objective\n{}\n\n\
         {criteria_section}\
         {checkpoint_section}\
         Workspace boundary\n\
         - You may ONLY modify files within the current workspace directory.\n\
         - You MUST NOT access files outside the workspace.\n\
         - You MUST NOT read .env, tokens, keys, or production secrets.\n\
         - You MUST NOT push, merge, or deploy code.\n\n\
         Testing requirements\n\
         - After making changes, run the project's test suite.\n\
         - All existing tests must continue to pass.\n\n\
         Checkpoint reporting\n\
         - At the very end of your output, emit a single JSON block (no markdown \
         fences) with exactly these keys:\n\
         {{\"findings\": \"...\", \"completed_steps\": [...], \
         \"remaining_steps\": [...], \"last_test_result\": \"...\", \
         \"blocker\": \"...\", \"next_action\": \"...\"}}\n\
         - When all work is done, set remaining_steps to [].\n\
         - Keep all other output concise.",
        job.objective,
    )
}

/// Extract the trailing checkpoint JSON from segment output. Accepts bare
/// JSON objects or ```json fenced blocks; the LAST valid object containing
/// `remaining_steps` wins.
fn parse_checkpoint(stdout: &str, workspace_root: &str) -> Option<Checkpoint> {
    let mut candidate: Option<Value> = None;
    let mut in_fence = false;
    let mut fenced = String::new();

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.starts_with("```json") || line.starts_with("```") && in_fence {
            if line.starts_with("```json") {
                in_fence = true;
                fenced.clear();
            } else {
                in_fence = false;
                if let Ok(v) = serde_json::from_str::<Value>(&fenced) {
                    if v.get("remaining_steps").is_some() {
                        candidate = Some(v);
                    }
                }
            }
            continue;
        }
        if in_fence {
            fenced.push_str(line);
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("remaining_steps").is_some() {
                candidate = Some(v);
            }
        }
    }

    let value = candidate?;
    let mut completed = Vec::new();
    if let Some(arr) = value.get("completed_steps").and_then(Value::as_array) {
        completed = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
    }
    let mut remaining = Vec::new();
    if let Some(arr) = value.get("remaining_steps").and_then(Value::as_array) {
        remaining = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
    }
    let str_of = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Some(Checkpoint {
        objective: str_of("objective"),
        boundaries: str_of("boundaries"),
        findings: str_of("findings"),
        workspace: crate::jobs::workspace_state(workspace_root),
        completed_steps: completed,
        remaining_steps: remaining,
        last_test_result: str_of("last_test_result"),
        blocker: str_of("blocker"),
        next_action: str_of("next_action"),
    })
}

/// Build the structured result Value (compatible with the previous task
/// status contract).
fn build_result(stdout: &str, objective: &str, exit_code: i32, timed_out: bool) -> Value {
    let (commit_sha, changed_files, diff_summary, test_command, test_result, summary) =
        parse_output(stdout, objective);
    serde_json::json!({
        "summary": summary,
        "commit_sha": commit_sha,
        "changed_files": changed_files,
        "diff_summary": diff_summary,
        "test_command": test_command,
        "test_result": test_result,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "stdout_truncated": truncate_str(stdout, MAX_OUTPUT),
        "stderr_truncated": "",
    })
}

fn parse_output(stdout: &str, objective: &str) -> (String, String, String, String, String, String) {
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
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(et) = event.get("type").and_then(|v| v.as_str()) {
                match et {
                    "completion" | "result" | "done" => {
                        if let Some(c) = event.get("content").and_then(|v| v.as_str()) {
                            summary = truncate_str(c, 200).to_string();
                        }
                    }
                    "file_change" | "edit" | "write" => {
                        if let Some(p) = event.get("path").and_then(|v| v.as_str()) {
                            if !changed_files.is_empty() {
                                changed_files.push_str(", ");
                            }
                            changed_files.push_str(p);
                        }
                    }
                    "diff" => {
                        if let Some(d) = event.get("diff").and_then(|v| v.as_str()) {
                            diff_summary = truncate_str(d, 500).to_string();
                        }
                    }
                    "test" | "test_result" => {
                        if let Some(s) = event.get("status").and_then(|v| v.as_str()) {
                            test_result = format!("test: {}", s);
                        }
                        if let Some(o) = event.get("output").and_then(|v| v.as_str()) {
                            test_result = truncate_str(o, 200).to_string();
                        }
                    }
                    "bash" | "tool_use" => {
                        if let Some(cmd_name) = event.pointer("/tool").and_then(|v| v.as_str()) {
                            if cmd_name == "bash" {
                                if let Some(input) = event
                                    .pointer("/state/input/command")
                                    .and_then(|v| v.as_str())
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
                if let Some(sha) = event.get("commit_sha").and_then(|v| v.as_str()) {
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

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn build_config_json() -> String {
    let config = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "permission": {
            "*": "deny",
            "read": "allow",
            "edit": "allow",
            "glob": "allow",
            "grep": "allow",
            "bash": "allow",
            "external_directory": "deny",
            "webfetch": "deny",
            "websearch": "deny",
            "task": "deny",
            "question": "deny"
        }
    });
    serde_json::to_string(&config).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobStatus, WorkspaceState};

    fn test_job(objective: &str) -> Job {
        Job {
            job_id: "job_test".into(),
            workspace_id: "test".into(),
            workspace_root: "/tmp/ws".into(),
            objective: objective.into(),
            acceptance_criteria: String::new(),
            backend: "opencode".into(),
            model: String::new(),
            status: JobStatus::Accepted,
            current_phase: String::new(),
            checkpoint: None,
            attempt: 1,
            created_at: String::new(),
            created_at_ms: 0,
            accepted_at: String::new(),
            updated_at: String::new(),
            last_error: String::new(),
            result_summary: String::new(),
            task_digest: String::new(),
            segments: Vec::new(),
            finalize: Default::default(),
            result: serde_json::Value::Null,
        }
    }

    #[test]
    fn config_uses_proper_allow_deny() {
        let config_str = build_config_json();
        let parsed: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let perm = &parsed["permission"];
        assert_eq!(perm["*"], "deny");
        assert_eq!(perm["read"], "allow");
        assert_eq!(perm["edit"], "allow");
        assert_eq!(perm["external_directory"], "deny");
        assert_eq!(perm["webfetch"], "deny");
        assert_eq!(perm["websearch"], "deny");
        assert_eq!(perm["task"], "deny");
        assert_eq!(perm["question"], "deny");
    }

    #[test]
    fn config_has_schema_url() {
        let config_str = build_config_json();
        let parsed: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        assert_eq!(parsed["$schema"], "https://opencode.ai/config.json");
    }

    #[test]
    fn prompt_includes_acceptance_criteria_and_checkpoint() {
        let job = test_job("do something");
        let mut job2 = job.clone();
        job2.acceptance_criteria = "must pass test X".into();
        let prompt = build_prompt(&job2, None);
        assert!(prompt.contains("Acceptance criteria"));
        assert!(prompt.contains("must pass test X"));
        assert!(prompt.contains("do something"));
        assert!(prompt.contains("Workspace boundary"));
        assert!(prompt.contains("remaining_steps"));
    }

    #[test]
    fn prompt_includes_previous_checkpoint() {
        let job = test_job("do something");
        let cp = Checkpoint {
            objective: "do something".into(),
            boundaries: "ws".into(),
            findings: "investigated X".into(),
            workspace: WorkspaceState {
                repository: String::new(),
                branch: String::new(),
                head: String::new(),
                working_tree_digest: String::new(),
            },
            completed_steps: vec!["added test".into()],
            remaining_steps: vec!["run cargo test".into()],
            last_test_result: "1 failed".into(),
            blocker: String::new(),
            next_action: "fix double(21)".into(),
        };
        let prompt = build_prompt(&job, Some(&cp));
        assert!(prompt.contains("Previous checkpoint"));
        assert!(prompt.contains("added test"));
        assert!(prompt.contains("fix double(21)"));
        assert!(prompt.contains("Do NOT redo completed work"));
    }

    #[test]
    fn parse_checkpoint_extracts_last_block() {
        let stdout = r#"{"type":"message","role":"assistant"}
{"findings":"f1","completed_steps":["a"],"remaining_steps":["b"],"last_test_result":"ok","blocker":"","next_action":"n1"}
{"type":"tool"}
```json
{"findings":"f2","completed_steps":["a","b"],"remaining_steps":[],"last_test_result":"ok","blocker":"","next_action":"done"}
```
"#;
        let cp = parse_checkpoint(stdout, "/tmp/ws").unwrap();
        assert_eq!(cp.findings, "f2");
        assert!(cp.remaining_steps.is_empty());
        assert_eq!(cp.completed_steps.len(), 2);
    }

    #[test]
    fn parse_checkpoint_none_when_missing() {
        assert!(parse_checkpoint("no json here\n", "/tmp/ws").is_none());
        assert!(
            parse_checkpoint(
                "{\"type\":\"message\",\"role\":\"assistant\"}\n",
                "/tmp/ws"
            )
            .is_none()
        );
    }

    #[test]
    fn monitor_counts_rounds_and_tools() {
        let monitor = EventMonitor::new();
        monitor.observe_event(&serde_json::json!({"type":"message","role":"assistant"}));
        monitor.observe_event(&serde_json::json!({"type":"message","role":"assistant"}));
        monitor.observe_event(&serde_json::json!({"type":"tool","tool_use":{}}));
        assert_eq!(monitor.rounds.load(Ordering::Relaxed), 2);
        assert_eq!(monitor.tools.load(Ordering::Relaxed), 1);
    }
}
