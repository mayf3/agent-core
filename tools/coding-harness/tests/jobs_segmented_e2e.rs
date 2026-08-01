//! E2E tests for the persistent segmented job engine (Development Harness V0).
//!
//! Covers: submit → accepted receipt, automatic multi-segment continuation
//! with checkpoints (no user "continue"), frozen budget hook decisions,
//! budget override rejection, restart recovery, drift detection, cancel,
//! and approval → resume.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BUDGET_RESTRICTED: &str = r#"{
    "max_model_rounds": 2,
    "max_wall_time_ms": 60000,
    "max_tool_calls": 10,
    "single_tool_timeout_ms": 30000,
    "on_exhaustion": "checkpoint_and_continue"
}"#;
const BUDGET_APPROVAL: &str = r#"{
    "max_model_rounds": 2,
    "max_wall_time_ms": 60000,
    "max_tool_calls": 10,
    "single_tool_timeout_ms": 30000,
    "on_exhaustion": "request_approval"
}"#;

struct HarnessServer {
    port: u16,
    ws_root: PathBuf,
    artifact_root: PathBuf,
}

impl HarnessServer {
    fn start() -> Self {
        // Task submit requires the control token; make tests self-contained.
        std::env::set_var("CODING_HARNESS_CONTROL_TOKEN", "test-harness-token");
        Self::start_with(None)
    }

    /// Start a harness whose job store lives in `store_override` (or a fresh
    /// temp dir when None). Workspace "test" uses the restricted segment
    /// budget; workspace "approval" uses request_approval.
    fn start_with(store_override: Option<&std::path::Path>) -> Self {
        std::env::set_var("CODING_HARNESS_CONTROL_TOKEN", "test-harness-token");
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let ws_root = std::env::temp_dir().join(format!("ch_jobs_ws_{}_{}", std::process::id(), ts));
        std::fs::create_dir_all(&ws_root).unwrap();
        let artifact_root = store_override
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("ch_jobs_art_{}_{}", std::process::id(), ts))
            });
        std::fs::create_dir_all(&artifact_root).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let mut workspaces = HashMap::new();
        let root = std::fs::canonicalize(&ws_root).unwrap_or_else(|_| ws_root.clone());
        let perm = coding_harness::config::WorkspacePermission {
            read: true,
            write: true,
            exec: true,
            opencode: false,
            network: false,
            shell: false,
        };
        workspaces.insert(
            "test".to_string(),
            coding_harness::config::WorkspaceEntry {
                root: root.clone(),
                perm: perm.clone(),
                segment_budget: Some(
                    serde_json::from_str(BUDGET_RESTRICTED).unwrap(),
                ),
            },
        );
        workspaces.insert(
            "approval".to_string(),
            coding_harness::config::WorkspaceEntry {
                root: root.clone(),
                perm: perm.clone(),
                segment_budget: Some(serde_json::from_str(BUDGET_APPROVAL).unwrap()),
            },
        );

        let config = Arc::new(coding_harness::config::CodingConfig {
            workspaces,
            kernel_api_url: "http://127.0.0.1:1".into(),
            capability_submit_token: "test-token".into(),
            artifact_root: artifact_root.clone(),
            hcr_profiles: HashMap::new(),
            hcr_token: String::new(),
        });
        let _sd = Arc::new(AtomicBool::new(false));
        coding_harness::jobs::start_scheduler(Arc::clone(&config));
        std::thread::spawn(move || {
            coding_harness::server::serve(listener, config);
        });
        std::thread::sleep(Duration::from_millis(200));
        Self {
            port,
            ws_root,
            artifact_root,
        }
    }

    fn request(&self, operation: &str, args: &Value) -> (u16, Value) {
        let body = json!({
            "protocol_version": "external-harness-v1",
            "operation": operation,
            "arguments": args,
        });
        let body_str = serde_json::to_string(&body).unwrap();
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let request = format!(
            "POST /execute HTTP/1.1\r\nAuthorization: Bearer test-harness-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n{}",
            body_str.len(), self.port, body_str
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        let response = String::from_utf8_lossy(&buf);
        let status_code: u16 = response
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        let parsed: Value = if let Some(pos) = response.find("\r\n\r\n") {
            serde_json::from_str(&response[pos + 4..]).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        (status_code, parsed)
    }

    fn submit(&self, workspace: &str, objective: &str, extra: Option<Value>) -> String {
        let mut args = json!({
            "workspace_id": workspace,
            "objective": objective,
            "acceptance_criteria": "fake acceptance",
            "backend": "fake",
        });
        if let Some(e) = extra {
            args["finalize"] = e;
        }
        let (code, body) = self.request("external.coding_task_submit", &args);
        assert_eq!(code, 200, "submit must be 200");
        assert_eq!(body["ok"], true, "submit must succeed: {body}");
        assert_eq!(body["result"]["status"], "accepted");
        body["result"]["job_id"].as_str().unwrap().to_string()
    }

    /// Poll status until `predicate` holds or timeout.
    fn poll<F: Fn(&Value) -> bool>(&self, job_id: &str, predicate: F, timeout: Duration) -> Value {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let (code, body) = self.request(
                "external.coding_task_status",
                &json!({"task_id": job_id}),
            );
            assert_eq!(code, 200);
            assert_eq!(body["ok"], true);
            if predicate(&body["result"]) {
                return body["result"].clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "poll timeout; last status: {}",
                body["result"]["status"]
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn wait_segment_count(hs: &HarnessServer, job_id: &str, n: u64) -> Value {
    hs.poll(
        job_id,
        move |r| r["segment_count"].as_u64().unwrap_or(0) >= n,
        Duration::from_secs(15),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Submit returns a real job_id + accepted receipt quickly, then the job
/// runs in bounded segments, checkpointing after each exhaustion and
/// automatically continuing — no user "continue" required.
#[test]
fn segmented_auto_continuation_with_checkpoints() {
    let hs = HarnessServer::start();
    let job_id = hs.submit("test", "fake_work_units:9", None);

    // Segment 1 exhausts (budget: 2 model rounds), checkpoint persisted.
    let after_first = wait_segment_count(&hs, &job_id, 1);
    assert_eq!(after_first["status"], "accepted", "job waits for next segment");
    assert_eq!(after_first["segment_count"], 1);
    assert_eq!(after_first["segments"][0]["outcome"], "exhausted");
    assert_eq!(
        after_first["segments"][0]["budget_frozen"]["hook_id"],
        "builtin:segment-budget-default-v0"
    );
    let digest = after_first["segments"][0]["budget_frozen"]["decision_digest"]
        .as_str()
        .unwrap_or("");
    assert!(!digest.is_empty(), "budget decision digest must be frozen");
    let cp = &after_first["checkpoint"];
    assert!(cp.is_object(), "checkpoint must be persisted");
    assert!(!cp["completed_steps"].as_array().unwrap().is_empty());
    assert!(!cp["remaining_steps"].as_array().unwrap().is_empty());

    // Second segment starts automatically (no user action).
    let after_second = wait_segment_count(&hs, &job_id, 2);
    assert!(after_second["attempt"].as_u64().unwrap() >= 2);
    assert_eq!(after_second["segments"][1]["outcome"], "exhausted");
    // Each segment froze the same hook identity but a distinct decision
    // digest (attempt differs).
    let digest2 = after_second["segments"][1]["budget_frozen"]["decision_digest"]
        .as_str()
        .unwrap_or("");
    assert_ne!(digest, digest2, "per-segment decision digest must differ");

    // Eventually completes on its own.
    let done = hs.poll(
        &job_id,
        |r| r["status"] == "completed",
        Duration::from_secs(30),
    );
    assert!(done["segment_count"].as_u64().unwrap() >= 3);
    assert_eq!(done["checkpoint"]["remaining_steps"].as_array().unwrap().len(), 0);
    assert_eq!(done["result"]["test_result"], "fake: passed");
    assert!(!done["summary"].as_str().unwrap_or("").is_empty());
    // Completion notification record persisted.
    let notify_dir = hs.artifact_root.join("jobs").join("notifications");
    let files: Vec<_> = std::fs::read_dir(&notify_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(&job_id))
        .collect();
    assert!(
        files.iter().any(|f| f.file_name().to_string_lossy().ends_with("_completed.json")),
        "completion notification record must be written"
    );
}

/// The model cannot override the resolved budget.
#[test]
fn budget_override_rejected() {    let hs = HarnessServer::start();
    let args = json!({
        "workspace_id": "test",
        "objective": "fake_work_units:5",
        "acceptance_criteria": "x",
        "backend": "fake",
        "segment_budget": {
            "max_model_rounds": 500,
            "max_wall_time_ms": 999999,
            "max_tool_calls": 999,
            "single_tool_timeout_ms": 999999,
            "on_exhaustion": "checkpoint_and_continue"
        },
    });
    let (code, body) = hs.request("external.coding_task_submit", &args);
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "budget_override_rejected");
}

/// A job that was `running` when the process died is recovered after
/// restart and continues to completion from its persisted store.
#[test]
fn restart_recovery_resumes_job() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let store_dir = std::env::temp_dir().join(format!("ch_jobs_recover_{}_{}", std::process::id(), ts));

    // Server A: submit, let segment 1 exhaust.
    let hs_a = HarnessServer::start_with(Some(&store_dir));
    let job_id = hs_a.submit("test", "fake_work_units:9", None);
    let after_first = wait_segment_count(&hs_a, &job_id, 1);
    assert_eq!(after_first["status"], "accepted");
    drop(hs_a);

    // Simulate crash mid-segment: flip the job back to `running` exactly as
    // the store would look if the process died during segment 2.
    let job_path = store_dir.join("jobs").join(format!("{job_id}.json"));
    let mut job: Value = serde_json::from_str(&std::fs::read_to_string(&job_path).unwrap()).unwrap();
    job["status"] = json!("running");
    job["current_phase"] = json!("segment_2");
    std::fs::write(&job_path, serde_json::to_vec_pretty(&job).unwrap()).unwrap();

    // "Restart": a fresh harness on the same store recovers and continues.
    let hs_b = HarnessServer::start_with(Some(&store_dir));
    let done = hs_b.poll(
        &job_id,
        |r| r["status"] == "completed" || r["status"] == "failed",
        Duration::from_secs(30),
    );
    assert_eq!(done["status"], "completed", "recovered job must complete");
    let receipts = done["segments"].as_array().unwrap();
    assert!(
        receipts.iter().any(|s| s["outcome"] == "interrupted"),
        "an interrupted receipt must record the crash"
    );
    assert!(
        receipts
            .iter()
            .any(|s| s["outcome"] == "exhausted" && s["reason"] == "max_model_rounds"),
        "checkpoint continuation must be visible in receipts"
    );
}

/// External workspace drift (HEAD/working tree changed between segments)
/// stops the job instead of blindly continuing.
#[test]
fn workspace_drift_stops_job() {
    let hs = HarnessServer::start();
    // Make the workspace a git repo so drift is detectable.
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(&hs.ws_root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    }

    let job_id = hs.submit("test", "fake_work_units:9", None);
    let after_first = wait_segment_count(&hs, &job_id, 1);
    assert_eq!(after_first["status"], "accepted");

    // External interference: a commit lands in the workspace between
    // segments (job must NOT continue blindly).
    let out = std::process::Command::new("git")
        .args(["commit", "-q", "--allow-empty", "-m", "external-change"])
        .current_dir(&hs.ws_root)
        .output()
        .unwrap();
    assert!(out.status.success());

    let done = hs.poll(
        &job_id,
        |r| r["status"] == "failed",
        Duration::from_secs(15),
    );
    assert!(
        done["last_error"].as_str().unwrap_or("").contains("checkpoint_drift"),
        "job must fail with checkpoint_drift, got: {}",
        done["last_error"]
    );
}

/// The model's own in-flight changes (uncommitted work at segment end) are
/// committed by the Harness as a checkpoint commit — the next segment must
/// continue instead of failing drift.
#[test]
fn checkpoint_commit_lands_model_work() {
    let hs = HarnessServer::start();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(&hs.ws_root)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    }

    // fake_dirty:true simulates a real model killed mid-work — the segment
    // leaves uncommitted workspace changes behind.
    let job_id = hs.submit("test", "fake_work_units:9 fake_dirty:true", None);
    let after_first = wait_segment_count(&hs, &job_id, 1);
    assert_eq!(after_first["status"], "accepted");

    // The job must auto-continue to completion: the Harness committed the
    // model's in-flight work, so the next segment sees no drift.
    let done = hs.poll(
        &job_id,
        |r| r["status"] == "completed" || r["status"] == "failed",
        Duration::from_secs(30),
    );
    assert_eq!(done["status"], "completed", "in-flight work must not drift-fail");
    let log = std::process::Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&hs.ws_root)
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(
        log.contains("checkpoint:"),
        "checkpoint commits must exist in git log: {log}"
    );
    let work = std::fs::read_to_string(hs.ws_root.join("model-work.txt"))
        .unwrap_or_default();
    assert!(
        work.starts_with("segment "),
        "model work file must be preserved by the checkpoint commits: {work:?}"
    );
    // Tree is clean after the final checkpoint commit.
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&hs.ws_root)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "",
        "working tree must be clean after checkpoint commits"
    );
}

/// Cancel stops a pending job; approval → resume continues it.
#[test]
fn cancel_and_resume() {
    let hs = HarnessServer::start();

    // Cancel a job that is waiting for its next segment.
    let cancel_id = hs.submit("test", "fake_work_units:9", None);
    wait_segment_count(&hs, &cancel_id, 1);
    let (code, body) = hs.request(
        "external.coding_task_cancel",
        &json!({"task_id": cancel_id}),
    );
    assert_eq!(code, 200);
    assert_eq!(body["result"]["status"], "cancelled");
    hs.poll(
        &cancel_id,
        |r| r["status"] == "cancelled",
        Duration::from_secs(10),
    );

    // request_approval budget → waiting_approval after each exhaustion;
    // each resume continues the job until it completes.
    let resume_id = hs.submit("approval", "fake_work_units:5", None);
    let mut resumes = 0;
    loop {
        let st = hs.poll(
            &resume_id,
            |r| {
                matches!(
                    r["status"].as_str(),
                    Some("waiting_approval" | "completed" | "failed")
                )
            },
            Duration::from_secs(15),
        );
        match st["status"].as_str() {
            Some("completed") => break,
            Some("waiting_approval") => {
                resumes += 1;
                let (code, body) = hs.request(
                    "external.coding_task_resume",
                    &json!({"task_id": resume_id}),
                );
                assert_eq!(code, 200);
                assert_eq!(body["result"]["status"], "accepted");
            }
            other => panic!("unexpected status: {other:?} ({st})"),
        }
        assert!(resumes <= 10, "too many approval cycles");
    }
    let done = hs.poll(
        &resume_id,
        |r| r["status"] == "completed",
        Duration::from_secs(30),
    );
    assert!(done["segment_count"].as_u64().unwrap() >= 2);
    assert!(resumes >= 2, "multi-segment approval flow expected");
}

/// A cancelled job cannot be cancelled again; unknown ids error cleanly.
#[test]
fn cancel_edge_cases() {
    let hs = HarnessServer::start();
    let (code, body) = hs.request("external.coding_task_cancel", &json!({"task_id": "job_unknown"}));
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "task_not_found");

    let job_id = hs.submit("test", "fake_work_units:9", None);
    wait_segment_count(&hs, &job_id, 1);
    let _ = hs.request("external.coding_task_cancel", &json!({"task_id": job_id}));
    let (code, body) = hs.request("external.coding_task_cancel", &json!({"task_id": job_id}));
    assert_eq!(code, 200);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error_code"], "task_not_cancellable");
}
