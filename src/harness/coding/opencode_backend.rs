//! OpenCode backend for coding.task.submit.
//!
//! Spawns `opencode run` with the given model and workspace directory.
//! Uses direct argv, no shell. Captures JSON output when available.
//! Timeout prevents unbounded execution.

use super::tasks::{truncate_str, TaskOutput};
use serde_json::Value;
use std::time::Duration;

pub const TIMEOUT_SECS: u64 = 600;
pub const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";

pub(crate) fn run_opencode(
    workspace_root: &str,
    objective: &str,
    acceptance_criteria: &str,
    model: &str,
) -> Result<TaskOutput, String> {
    let opencode_path = find_opencode().map_err(|e| format!("opencode_not_found: {e}"))?;
    let prompt = build_prompt(objective, acceptance_criteria);

    let resolved_model = if model.is_empty() {
        DEFAULT_MODEL
    } else {
        model
    };

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
    for var in &[
        "PATH",
        "HOME",
        "TMPDIR",
        "OPENCODE_SERVER_PASSWORD",
        "OPENCODE_SERVER_USERNAME",
        "DEEPSEEK_API_KEY",
    ] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let deadline = Duration::from_secs(TIMEOUT_SECS);
    let start = std::time::Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("opencode_spawn_failed: {e}"))?;

    let (exit_code, stdout_str, stderr_str, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                let _ = child
                    .stdout
                    .take()
                    .map(|mut s| std::io::Read::read_to_end(&mut s, &mut stdout));
                let _ = child
                    .stderr
                    .take()
                    .map(|mut s| std::io::Read::read_to_end(&mut s, &mut stderr));
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
            let _ = child.kill();
            let _ = child.wait();
            break (-1, String::new(), "timed_out".to_string(), true);
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    if exit_code == 0 {
        let (commit_sha, diff_summary, test_result, summary) =
            parse_output(&stdout_str, objective, acceptance_criteria);
        Ok(TaskOutput {
            summary,
            commit_sha,
            diff_summary,
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

pub(crate) fn find_opencode() -> Result<String, String> {
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

fn build_prompt(objective: &str, acceptance_criteria: &str) -> String {
    format!(
        "Objective\n{objective}\n\n\
         Acceptance criteria\n{acceptance_criteria}\n\n\
         Workspace boundary\n\
         - You may ONLY modify files within the current workspace directory.\n\
         - You MUST NOT access files outside the workspace.\n\
         - You MUST NOT read .env, tokens, keys, or production secrets.\n\
         - You MUST NOT push, merge, or deploy code.\n\n\
         Testing requirements\n\
         - After making changes, run the project's test suite.\n\
         - All existing tests must continue to pass.\n\
         - If adding new functionality, include tests.\n\n\
         Completion reporting\n\
         - Report which files were changed and why.\n\
         - Report test results and any failures.\n\
         - Keep output concise.\n\n\
         Security boundaries\n\
         - Do not expose credentials, API keys, or tokens.\n\
         - Do not make network requests to unknown hosts.\n\
         - Do not modify system configuration outside the workspace."
    )
}

fn parse_output(
    stdout: &str,
    objective: &str,
    acceptance_criteria: &str,
) -> (String, String, String, String) {
    let mut commit_sha = String::new();
    let mut diff_summary = String::new();
    let mut test_result = String::new();
    let mut summary = format!("opencode: completed '{}'", truncate_str(objective, 80));

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
            if let Some(event_type) = event.get("type").and_then(Value::as_str) {
                match event_type {
                    "completion" | "result" | "done" => {
                        if let Some(content) = event.get("content").and_then(Value::as_str) {
                            summary = truncate_str(content, 200).to_string();
                        }
                    }
                    "file_change" | "edit" | "write" => {
                        if let Some(path) = event.get("path").and_then(Value::as_str) {
                            if !diff_summary.is_empty() {
                                diff_summary.push_str(", ");
                            }
                            diff_summary.push_str(path);
                        }
                    }
                    "diff" => {
                        if let Some(diff_text) = event.get("diff").and_then(Value::as_str) {
                            diff_summary = truncate_str(diff_text, 500).to_string();
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
                    _ => {}
                }
            }
            if commit_sha.is_empty() {
                if let Some(sha) = event.get("commit_sha").and_then(Value::as_str) {
                    commit_sha = sha.to_string();
                }
            }
            if test_result.is_empty() {
                if let Some(tr) = event.get("test_result").and_then(Value::as_str) {
                    test_result = tr.to_string();
                }
                if let Some(to) = event.get("test_output").and_then(Value::as_str) {
                    test_result = truncate_str(to, 200).to_string();
                }
            }
        }
    }

    if diff_summary.is_empty() {
        diff_summary = "opencode: see stdout for changes".to_string();
    }
    if test_result.is_empty() {
        test_result = format!(
            "acceptance: '{}' (exit=0)",
            truncate_str(acceptance_criteria, 80)
        );
    }

    (commit_sha, diff_summary, test_result, summary)
}
