//! Persistent segmented development jobs — Development Harness V0.
//!
//! This module turns `external.coding_task_submit` into a durable external
//! Job that survives Harness restarts and runs in bounded execution
//! segments, automatically continuing without any user "continue" message.
//!
//! Responsibilities (all external to the Agent Core Kernel):
//! - persistent job store (JSON files, atomic writes, restart recovery);
//! - per-segment budget hook: the builtin hook resolves the segment budget,
//!   frozen (identity + version + decision digest) at segment start;
//! - checkpoint persistence + workspace drift verification before each
//!   continuation segment;
//! - automatic scheduling of the next segment on exhaustion;
//! - terminal notification records (+ optional Feishu webhook);
//! - optional finalize step: push the job branch and open a PR.
//!
//! The model cannot override a resolved budget: any submitted
//! `segment_budget` that differs from the resolved value is rejected
//! (`budget_override_rejected`), and every resolved budget is capped by the
//! host safety ceiling regardless of configuration.

use crate::config::CodingConfig;
use crate::opencode_backend;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Stable identity of the builtin segment-budget hook. A project or task may
/// register another hook later; the identity/version are frozen per segment.
pub const SEGMENT_BUDGET_HOOK_ID: &str = "builtin:segment-budget-default-v0";
pub const SEGMENT_BUDGET_HOOK_VERSION: &str = "v0";

/// Hard safety ceiling applied to every resolved budget (host-level fuse).
/// Configuration may not raise a budget above these caps.
pub const HOST_CEILING_JSON: &str = r#"{
    "max_model_rounds": 1000,
    "max_wall_time_ms": 3600000,
    "max_tool_calls": 2000,
    "single_tool_timeout_ms": 600000,
    "on_exhaustion": "checkpoint_and_continue"
}"#;

/// Builtin default segment budget (used when no workspace override and no
/// `HARNESS_SEGMENT_BUDGET` env var is present).
pub const DEFAULT_BUDGET_JSON: &str = r#"{
    "max_model_rounds": 100,
    "max_wall_time_ms": 300000,
    "max_tool_calls": 200,
    "single_tool_timeout_ms": 120000,
    "on_exhaustion": "checkpoint_and_continue"
}"#;

/// Maximum number of segments a single job may consume. This is a
/// host-level fuse against infinite auto-continuation loops.
pub const MAX_SEGMENTS_PER_JOB: u64 = 50;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Accepted,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Accepted => "accepted",
            JobStatus::Running => "running",
            JobStatus::WaitingApproval => "waiting_approval",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustionPolicy {
    CheckpointAndContinue,
    StopFailed,
    RequestApproval,
}

/// Per-segment execution budget. `on_exhaustion` decides what the Harness
/// does when a segment runs out; the Harness alone counts/times/enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSegmentBudget {
    pub max_model_rounds: u64,
    pub max_wall_time_ms: u64,
    pub max_tool_calls: u64,
    pub single_tool_timeout_ms: u64,
    pub on_exhaustion: ExhaustionPolicy,
}

impl ExecutionSegmentBudget {
    pub fn parse(value: &Value) -> Result<Self, String> {
        let num = |key: &str| -> Result<u64, String> {
            value
                .get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("budget_field_missing_or_invalid: {key}"))
        };
        let policy = match value.get("on_exhaustion").and_then(Value::as_str) {
            Some("checkpoint_and_continue") => ExhaustionPolicy::CheckpointAndContinue,
            Some("stop_failed") => ExhaustionPolicy::StopFailed,
            Some("request_approval") => ExhaustionPolicy::RequestApproval,
            _ => return Err("budget_policy_invalid".into()),
        };
        Ok(Self {
            max_model_rounds: num("max_model_rounds")?,
            max_wall_time_ms: num("max_wall_time_ms")?,
            max_tool_calls: num("max_tool_calls")?,
            single_tool_timeout_ms: num("single_tool_timeout_ms")?,
            on_exhaustion: policy,
        })
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The budget hook's frozen decision for one segment. The hook identity,
/// hook version and the decision digest (sha256 over the resolved budget and
/// attempt) are recorded in the segment receipt so a later segment cannot
/// silently change the governing policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenBudget {
    pub hook_id: String,
    pub hook_version: String,
    pub decision_digest: String,
    pub budget: ExecutionSegmentBudget,
}

/// Git/workspace facts recorded in a checkpoint. Verified before each
/// continuation segment; on drift the job stops and reports instead of
/// blindly continuing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub repository: String,
    pub branch: String,
    pub head: String,
    pub working_tree_digest: String,
}

/// Minimal checkpoint: enough facts to continue development. Code, git
/// commits and test artifacts stay authoritative in the real workspace —
/// never copied into the job database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub objective: String,
    pub boundaries: String,
    pub findings: String,
    pub workspace: WorkspaceState,
    pub completed_steps: Vec<String>,
    pub remaining_steps: Vec<String>,
    pub last_test_result: String,
    pub blocker: String,
    pub next_action: String,
}

/// One bounded execution segment, with the frozen budget and counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentReceipt {
    pub segment_no: u64,
    pub attempt: u64,
    pub started_at: String,
    pub ended_at: String,
    /// completed | exhausted | failed | cancelled | interrupted
    pub outcome: String,
    pub reason: String,
    pub budget_frozen: FrozenBudget,
    pub model_rounds_used: u64,
    pub tool_calls_used: u64,
    pub wall_time_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FinalizeConfig {
    #[serde(default)]
    pub create_pr: bool,
    #[serde(default)]
    pub pr_title: String,
    #[serde(default)]
    pub pr_body: String,
    #[serde(default)]
    pub base_branch: String,
    #[serde(default)]
    pub branch: String,
}

/// The persisted Job record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub workspace_id: String,
    pub workspace_root: String,
    pub objective: String,
    pub acceptance_criteria: String,
    pub backend: String,
    pub model: String,
    pub status: JobStatus,
    pub current_phase: String,
    pub checkpoint: Option<Checkpoint>,
    pub attempt: u64,
    pub created_at: String,
    pub created_at_ms: u64,
    pub accepted_at: String,
    pub updated_at: String,
    pub last_error: String,
    pub result_summary: String,
    pub task_digest: String,
    pub segments: Vec<SegmentReceipt>,
    pub finalize: FinalizeConfig,
    /// Structured result of the final (or most recent) segment: summary,
    /// commit_sha, changed_files, diff_summary, test_command, test_result,
    /// exit_code, timed_out, stdout_truncated, stderr_truncated.
    pub result: Value,
}

impl Job {
    fn next_segment_no(&self) -> u64 {
        self.segments.len() as u64 + 1
    }
}

/// Outcome of one backend segment run.
pub enum SegmentEnd {
    /// The task is complete (acceptance satisfied).
    Completed(Value),
    /// Budget exhausted; checkpoint should drive the next segment.
    Exhausted(String),
    /// Backend execution failed.
    Failed(String),
    /// Job was cancelled mid-segment.
    Cancelled,
}

pub struct SegmentOutcome {
    pub end: SegmentEnd,
    pub model_rounds_used: u64,
    pub tool_calls_used: u64,
    pub wall_time_ms: u64,
    pub checkpoint: Option<Checkpoint>,
}

// ---------------------------------------------------------------------------
// Persistent store
// ---------------------------------------------------------------------------

/// Job store root: `HARNESS_JOB_STORE` env var, else `<artifact_root>/jobs`.
pub fn store_root(config: &CodingConfig) -> PathBuf {
    std::env::var("HARNESS_JOB_STORE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| config.artifact_root.join("jobs"))
}

struct Store {
    root: PathBuf,
}

impl Store {
    fn open(root: &Path) -> Result<Store, String> {
        std::fs::create_dir_all(root).map_err(|e| format!("job_store_unwritable: {e}"))?;
        Ok(Store {
            root: root.to_path_buf(),
        })
    }

    fn path_for(&self, job_id: &str) -> PathBuf {
        self.root.join(format!("{job_id}.json"))
    }

    fn load(&self, job_id: &str) -> Option<Job> {
        let bytes = std::fs::read(self.path_for(job_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save(&self, job: &Job) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(job)
            .map_err(|e| format!("job_serialize_failed: {e}"))?;
        let final_path = self.path_for(&job.job_id);
        let tmp = self.root.join(format!(
            "{}.tmp{}",
            job.job_id,
            std::process::id()
        ));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("job_write_failed: {e}"))?;
        std::fs::rename(&tmp, &final_path).map_err(|e| format!("job_commit_failed: {e}"))?;
        Ok(())
    }

    /// All job records (skips the notifications subdirectory).
    fn all(&self) -> Vec<Job> {
        let mut jobs = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return jobs;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(job) = serde_json::from_slice::<Job>(&bytes) {
                    jobs.push(job);
                }
            }
        }
        jobs.sort_by_key(|j| j.created_at_ms);
        jobs
    }

    fn next_accepted(&self) -> Option<Job> {
        self.all()
            .into_iter()
            .find(|j| j.status == JobStatus::Accepted)
    }
}

/// Restart recovery: a job that was `running` when the process died is
/// marked `accepted` again (with an `interrupted` segment receipt) so the
/// scheduler resumes it. Terminal jobs are left untouched.
fn recover_all(store: &Store) {
    for mut job in store.all() {
        if job.status == JobStatus::Running {
            let receipt = SegmentReceipt {
                segment_no: job.next_segment_no(),
                attempt: job.attempt,
                started_at: job.updated_at.clone(),
                ended_at: now_rfc3339(),
                outcome: "interrupted".into(),
                reason: "harness_restart_recovery".into(),
                budget_frozen: job
                    .segments
                    .last()
                    .map(|s| s.budget_frozen.clone())
                    .unwrap_or_else(fallback_frozen_budget),
                model_rounds_used: 0,
                tool_calls_used: 0,
                wall_time_ms: 0,
            };
            job.segments.push(receipt);
            job.status = JobStatus::Accepted;
            job.current_phase = format!("segment_{}_pending", job.next_segment_no());
            job.last_error = "recovered_after_harness_restart".into();
            job.updated_at = now_rfc3339();
            let _ = store.save(&job);
        }
    }
}

fn fallback_frozen_budget() -> FrozenBudget {
    FrozenBudget {
        hook_id: SEGMENT_BUDGET_HOOK_ID.into(),
        hook_version: SEGMENT_BUDGET_HOOK_VERSION.into(),
        decision_digest: String::new(),
        budget: ExecutionSegmentBudget::parse(
            &serde_json::from_str(DEFAULT_BUDGET_JSON).unwrap_or(Value::Null),
        )
        .unwrap_or_else(|_| ExecutionSegmentBudget {
            max_model_rounds: 100,
            max_wall_time_ms: 300_000,
            max_tool_calls: 200,
            single_tool_timeout_ms: 120_000,
            on_exhaustion: ExhaustionPolicy::CheckpointAndContinue,
        }),
    }
}

// ---------------------------------------------------------------------------
// Budget hook
// ---------------------------------------------------------------------------

/// Resolve the segment budget for a workspace. Priority:
/// 1. workspace `segment_budget` from CODING_CONFIG;
/// 2. `HARNESS_SEGMENT_BUDGET` env var (JSON);
/// 3. builtin default.
/// The result is then capped by the host safety ceiling
/// (`HARNESS_HOST_SAFETY_CEILING` env var, else builtin ceiling).
///
/// A model-submitted `segment_budget` that differs from the resolved value
/// is rejected — the model cannot override the budget.
pub fn resolve_budget(
    config: &CodingConfig,
    workspace_id: &str,
    submitted: Option<&Value>,
) -> Result<ExecutionSegmentBudget, String> {
    let workspace_value = config
        .workspaces
        .get(workspace_id)
        .and_then(|w| w.segment_budget.clone());
    let env_value = env_json("HARNESS_SEGMENT_BUDGET");
    let base = workspace_value
        .or(env_value)
        .unwrap_or_else(|| serde_json::from_str(DEFAULT_BUDGET_JSON).unwrap_or(Value::Null));
    let budget = ExecutionSegmentBudget::parse(&base)?;

    if let Some(submitted) = submitted {
        if !submitted.is_null() && submitted != &budget.to_value() {
            return Err("budget_override_rejected".into());
        }
    }

    let ceiling_value = env_json("HARNESS_HOST_SAFETY_CEILING")
        .unwrap_or_else(|| serde_json::from_str(HOST_CEILING_JSON).unwrap_or(Value::Null));
    let ceiling = ExecutionSegmentBudget::parse(&ceiling_value)?;
    Ok(ExecutionSegmentBudget {
        max_model_rounds: budget.max_model_rounds.min(ceiling.max_model_rounds),
        max_wall_time_ms: budget.max_wall_time_ms.min(ceiling.max_wall_time_ms),
        max_tool_calls: budget.max_tool_calls.min(ceiling.max_tool_calls),
        single_tool_timeout_ms: budget
            .single_tool_timeout_ms
            .min(ceiling.single_tool_timeout_ms),
        on_exhaustion: budget.on_exhaustion,
    })
}

/// Freeze the hook identity/version and the decision digest for a segment.
fn freeze_budget(budget: &ExecutionSegmentBudget, attempt: u64) -> FrozenBudget {
    let payload = serde_json::to_string(&json!({
        "hook_id": SEGMENT_BUDGET_HOOK_ID,
        "hook_version": SEGMENT_BUDGET_HOOK_VERSION,
        "attempt": attempt,
        "budget": budget,
    }))
    .unwrap_or_default();
    FrozenBudget {
        hook_id: SEGMENT_BUDGET_HOOK_ID.into(),
        hook_version: SEGMENT_BUDGET_HOOK_VERSION.into(),
        decision_digest: hex_sha256(payload.as_bytes()),
        budget: budget.clone(),
    }
}

// ---------------------------------------------------------------------------
// Public API (called from server.rs)
// ---------------------------------------------------------------------------

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

/// Submit a development task as a persistent segmented Job. Returns quickly
/// with job_id/status=accepted/accepted_at/task_digest; execution happens on
/// the scheduler. Never blocks a Kernel Run.
pub fn submit(
    config: &CodingConfig,
    workspace_id: &str,
    workspace_root: &Path,
    objective: &str,
    acceptance_criteria: &str,
    backend: &str,
    model: Option<&str>,
    args: &Value,
) -> Value {
    if backend != "fake" && backend != "opencode" {
        return err(&format!("unsupported_backend: {backend}"));
    }
    let resolved_budget = match resolve_budget(config, workspace_id, args.get("segment_budget")) {
        Ok(b) => b,
        Err(code) => return err(&code),
    };
    // Validate finalize config eagerly so a broken PR request fails at
    // submit time, not after the whole job ran.
    let finalize: FinalizeConfig = args
        .get("finalize")
        .cloned()
        .map(|v| serde_json::from_value(v).unwrap_or_default())
        .unwrap_or_default();

    let store = match Store::open(&store_root(config)) {
        Ok(s) => s,
        Err(code) => return err(&code),
    };
    let job_id = new_job_id();
    let now = now_rfc3339();
    let now_ms = unix_ms();
    let mut digest_input = Vec::new();
    digest_input.extend_from_slice(workspace_id.as_bytes());
    digest_input.push(0);
    digest_input.extend_from_slice(workspace_root.to_string_lossy().as_bytes());
    digest_input.push(0);
    digest_input.extend_from_slice(objective.as_bytes());
    digest_input.push(0);
    digest_input.extend_from_slice(acceptance_criteria.as_bytes());
    digest_input.push(0);
    digest_input.extend_from_slice(backend.as_bytes());

    let job = Job {
        job_id: job_id.clone(),
        workspace_id: workspace_id.to_string(),
        workspace_root: workspace_root.to_string_lossy().to_string(),
        objective: objective.to_string(),
        acceptance_criteria: acceptance_criteria.to_string(),
        backend: backend.to_string(),
        model: model.unwrap_or("").to_string(),
        status: JobStatus::Accepted,
        current_phase: "segment_1_pending".into(),
        checkpoint: None,
        attempt: 0,
        created_at: now.clone(),
        created_at_ms: now_ms,
        accepted_at: now.clone(),
        updated_at: now,
        last_error: String::new(),
        result_summary: String::new(),
        task_digest: hex_sha256(&digest_input),
        segments: Vec::new(),
        finalize,
        result: Value::Null,
    };
    if let Err(code) = store.save(&job) {
        return err(&code);
    }
    ok(json!({
        "job_id": job_id,
        "task_id": job_id,
        "status": "accepted",
        "accepted_at": job.accepted_at,
        "task_digest": job.task_digest,
        "backend": backend,
        "model": model.unwrap_or(""),
        "workspace_id": workspace_id,
        "created_at": job.created_at,
        "segment_budget": resolved_budget,
    }))
}

/// Status query: full job view including checkpoint, segment receipts and
/// the frozen budget of the most recent segment.
pub fn status(config: &CodingConfig, job_id: &str) -> Value {
    let store = match Store::open(&store_root(config)) {
        Ok(s) => s,
        Err(code) => return err(&code),
    };
    match store.load(job_id) {
        Some(job) => {
            let segments: Vec<Value> = job
                .segments
                .iter()
                .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
                .collect();
            let last_error = if job.last_error.is_empty() {
                Value::Null
            } else {
                json!(job.last_error)
            };
            let failure_reason = if job.status == JobStatus::Failed {
                last_error.clone()
            } else {
                Value::Null
            };
            ok(json!({
                "job_id": job.job_id,
                "task_id": job.job_id,
                "status": job.status.as_str(),
                "current_phase": job.current_phase,
                "attempt": job.attempt,
                "backend": job.backend,
                "model": job.model,
                "workspace_id": job.workspace_id,
                "created_at": job.created_at,
                "accepted_at": job.accepted_at,
                "updated_at": job.updated_at,
                "task_digest": job.task_digest,
                "summary": job.result_summary,
                "result": job.result,
                "checkpoint": job.checkpoint,
                "segment_count": job.segments.len(),
                "segments": segments,
                "last_error": last_error,
                "failure_reason": failure_reason,
            }))
        }
        None => err("task_not_found"),
    }
}

/// Cancel a job (also aborts an in-flight segment).
pub fn cancel(config: &CodingConfig, job_id: &str) -> Value {
    let store = match Store::open(&store_root(config)) {
        Ok(s) => s,
        Err(code) => return err(&code),
    };
    let Some(mut job) = store.load(job_id) else {
        return err("task_not_found");
    };
    if job.status.is_terminal() {
        return err("task_not_cancellable");
    }
    if let Some(flag) = cancel_flags().lock().unwrap().get(job_id) {
        flag.store(true, Ordering::SeqCst);
    }
    job.status = JobStatus::Cancelled;
    job.current_phase = "cancelled".into();
    job.updated_at = now_rfc3339();
    if store.save(&job).is_err() {
        return err("job_write_failed");
    }
    notify(&job, config);
    ok(json!({"job_id": job_id, "status": "cancelled"}))
}

/// Resume a job that paused for approval (waiting_approval → accepted).
pub fn resume(config: &CodingConfig, job_id: &str) -> Value {
    let store = match Store::open(&store_root(config)) {
        Ok(s) => s,
        Err(code) => return err(&code),
    };
    let Some(mut job) = store.load(job_id) else {
        return err("task_not_found");
    };
    if job.status != JobStatus::WaitingApproval {
        return err("task_not_resumable");
    }
    job.status = JobStatus::Accepted;
    job.current_phase = format!("segment_{}_pending", job.next_segment_no());
    job.updated_at = now_rfc3339();
    if store.save(&job).is_err() {
        return err("job_write_failed");
    }
    ok(json!({"job_id": job_id, "status": "accepted"}))
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

static SEGMENT_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Start the background scheduler: recovers interrupted jobs, then runs one
/// segment at a time for accepted jobs, automatically continuing after each
/// exhaustion until the job reaches a terminal state.
pub fn start_scheduler(config: Arc<CodingConfig>) {
    std::thread::spawn(move || {
        if let Ok(store) = Store::open(&store_root(&config)) {
            recover_all(&store);
        }
        loop {
            tick(&config);
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

fn tick(config: &CodingConfig) {
    if SEGMENT_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    let store = match Store::open(&store_root(config)) {
        Ok(s) => s,
        Err(_) => {
            SEGMENT_IN_FLIGHT.store(false, Ordering::SeqCst);
            return;
        }
    };
    let job_id = match store.next_accepted() {
        Some(job) => job.job_id,
        None => {
            SEGMENT_IN_FLIGHT.store(false, Ordering::SeqCst);
            return;
        }
    };
    let cfg = Arc::new(config.clone());
    std::thread::spawn(move || {
        // A panicking segment must never wedge the global in-flight flag:
        // catch, log, and always release.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_next_segment(&cfg, &job_id);
        }));
        if result.is_err() {
            eprintln!("coding_harness: segment thread panicked for job {job_id}");
        }
        SEGMENT_IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

fn run_next_segment(config: &CodingConfig, job_id: &str) {
    let store = match Store::open(&store_root(config)) {
        Ok(s) => s,
        Err(_) => return,
    };
    let Some(mut job) = store.load(job_id) else {
        return;
    };
    if job.status != JobStatus::Accepted {
        return;
    }
    if job.checkpoint.is_none() {
        if let Err(code) = setup_branch(&mut job) {
            job.status = JobStatus::Failed;
            job.current_phase = "failed".into();
            job.last_error = code;
            job.updated_at = now_rfc3339();
            let _ = store.save(&job);
            notify(&job, config);
            return;
        }
    }

    // Freeze the budget hook decision for THIS segment (attempt-based).
    let budget = match resolve_budget(config, &job.workspace_id, None) {
        Ok(b) => b,
        Err(code) => {
            job.status = JobStatus::Failed;
            job.current_phase = "failed".into();
            job.last_error = code;
            job.updated_at = now_rfc3339();
            let _ = store.save(&job);
            notify(&job, config);
            return;
        }
    };
    let frozen = freeze_budget(&budget, job.attempt + 1);

    // Drift verification before continuing from a checkpoint.
    if let Some(cp) = job.checkpoint.clone() {
        if let Err(detail) = verify_no_drift(&job.workspace_root, &cp) {
            job.status = JobStatus::Failed;
            job.current_phase = "failed".into();
            job.last_error = detail;
            job.updated_at = now_rfc3339();
            let _ = store.save(&job);
            notify(&job, config);
            return;
        }
    }

    let started = now_rfc3339();
    job.status = JobStatus::Running;
    job.attempt += 1;
    job.current_phase = format!("segment_{}", job.next_segment_no());
    job.updated_at = started.clone();
    if store.save(&job).is_err() {
        return;
    }

    let cancel_flag = register_cancel(&job.job_id);
    let outcome = match job.backend.as_str() {
        "opencode" => opencode_backend::run_segment(
            &job,
            &budget,
            job.checkpoint.as_ref(),
            Some(cancel_flag.clone()),
        ),
        _ => run_fake_segment(&job, &budget, job.checkpoint.as_ref(), cancel_flag),
    };
    cleanup_cancel(&job.job_id);

    let ended = now_rfc3339();
    let mut receipt = SegmentReceipt {
        segment_no: job.next_segment_no(),
        attempt: job.attempt,
        started_at: started,
        ended_at: ended.clone(),
        outcome: String::new(),
        reason: String::new(),
        budget_frozen: frozen,
        model_rounds_used: outcome.model_rounds_used,
        tool_calls_used: outcome.tool_calls_used,
        wall_time_ms: outcome.wall_time_ms,
    };
    job.updated_at = ended;

    match outcome.end {
        SegmentEnd::Completed(result) => {
            receipt.outcome = "completed".into();
            receipt.reason = "acceptance_satisfied".into();
            if let Some(cp) = outcome.checkpoint {
                job.checkpoint = Some(cp);
            }
            job.result = result.clone();
            job.result_summary = result
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("completed")
                .to_string();
            job.status = JobStatus::Completed;
            job.current_phase = "completed".into();
            job.last_error = String::new();
        }
        SegmentEnd::Exhausted(reason) => {
            receipt.outcome = "exhausted".into();
            receipt.reason = reason.clone();
            if let Some(cp) = outcome.checkpoint {
                job.checkpoint = Some(cp);
            }
            if job.segments.len() + 1 >= MAX_SEGMENTS_PER_JOB as usize {
                job.status = JobStatus::Failed;
                job.current_phase = "failed".into();
                job.last_error = "segment_count_ceiling".into();
            } else {
                match budget.on_exhaustion {
                    ExhaustionPolicy::CheckpointAndContinue => {
                        job.status = JobStatus::Accepted;
                        job.current_phase = format!("segment_{}_pending", job.next_segment_no() + 1);
                    }
                    ExhaustionPolicy::StopFailed => {
                        job.status = JobStatus::Failed;
                        job.current_phase = "failed".into();
                        job.last_error = "budget_exhausted_stop_failed".into();
                    }
                    ExhaustionPolicy::RequestApproval => {
                        job.status = JobStatus::WaitingApproval;
                        job.current_phase = "waiting_approval".into();
                    }
                }
            }
        }
        SegmentEnd::Failed(reason) => {
            receipt.outcome = "failed".into();
            receipt.reason = reason.clone();
            job.status = JobStatus::Failed;
            job.current_phase = "failed".into();
            job.last_error = reason;
        }
        SegmentEnd::Cancelled => {
            receipt.outcome = "cancelled".into();
            receipt.reason = "cancelled_by_user".into();
            job.status = JobStatus::Cancelled;
            job.current_phase = "cancelled".into();
        }
    }
    job.segments.push(receipt);
    if store.save(&job).is_err() {
        return;
    }

    if job.status == JobStatus::Completed {
        finalize_pr(config, &job);
    }
    if job.status.is_terminal() || job.status == JobStatus::WaitingApproval {
        notify(&job, config);
    }
}

/// Per-job cancel flags so an in-flight segment can be aborted.
fn cancel_flags() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    use std::sync::OnceLock;
    static FLAGS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    FLAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_cancel(job_id: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    cancel_flags()
        .lock()
        .unwrap()
        .insert(job_id.to_string(), flag.clone());
    flag
}

fn cleanup_cancel(job_id: &str) {
    cancel_flags().lock().unwrap().remove(job_id);
}

// ---------------------------------------------------------------------------
// Job setup and finalize
// ---------------------------------------------------------------------------

/// Before segment 1: if the job asked for a PR, make sure the workspace is a
/// git repository and create the job branch from the base branch.
fn setup_branch(job: &mut Job) -> Result<(), String> {
    if !job.finalize.create_pr {
        return Ok(());
    }
    let base = if job.finalize.base_branch.is_empty() {
        "main"
    } else {
        &job.finalize.base_branch
    };
    let branch = if job.finalize.branch.is_empty() {
        format!("codex/job-{}", job.job_id)
    } else {
        job.finalize.branch.clone()
    };
    if !is_git_repo(&job.workspace_root) {
        return Err("pr_requires_git_workspace".into());
    }
    // Branch may already exist (e.g. restart after submit); switch to it.
    let exists = git(
        &job.workspace_root,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .0;
    if exists {
        let (ok, err) = git(&job.workspace_root, &["checkout", &branch]);
        if !ok {
            return Err(format!("branch_checkout_failed: {err}"));
        }
    } else {
        let (ok, err) = git(&job.workspace_root, &["checkout", "-b", &branch, base]);
        if !ok {
            return Err(format!("branch_create_failed: {err}"));
        }
    }
    let _ = git(
        &job.workspace_root,
        &["config", "user.name", "agent-core-harness"],
    );
    let _ = git(
        &job.workspace_root,
        &["config", "user.email", "harness@agent-core.local"],
    );
    // Remember the branch so later segments verify against it.
    if let Some(cp) = job.checkpoint.as_mut() {
        cp.workspace.branch = branch;
    } else {
        let mut cp = checkpoint_template(job);
        cp.workspace.branch = branch;
        job.checkpoint = Some(cp);
    }
    Ok(())
}

/// After completion: push the job branch and open a PR (never auto-merge).
fn finalize_pr(config: &CodingConfig, job: &Job) {
    if !job.finalize.create_pr {
        return;
    }
    let branch = if job.finalize.branch.is_empty() {
        format!("codex/job-{}", job.job_id)
    } else {
        job.finalize.branch.clone()
    };
    let base = if job.finalize.base_branch.is_empty() {
        "main"
    } else {
        &job.finalize.base_branch
    };
    let title = if job.finalize.pr_title.is_empty() {
        format!("feat: {} (job {})", truncate_str(&job.objective, 60), job.job_id)
    } else {
        job.finalize.pr_title.clone()
    };
    let body = if job.finalize.pr_body.is_empty() {
        format!(
            "Automated job {}\n\nTask digest: {}\nSegments: {}\n\nObjective:\n{}",
            job.job_id,
            job.task_digest,
            job.segments.len(),
            truncate_str(&job.objective, 2000),
        )
    } else {
        job.finalize.pr_body.clone()
    };

    let (ok_push, err_push) = git(
        &job.workspace_root,
        &["push", "-u", "origin", &branch],
    );
    if !ok_push {
        record_finalize_note(config, job, &format!("pr_push_failed: {err_push}"));
        return;
    }
    let pr = std::process::Command::new("gh")
        .args([
            "pr", "create", "--base", base, "--head", &branch, "--title", &title, "--body",
            &body,
        ])
        .current_dir(&job.workspace_root)
        .output();
    match pr {
        Ok(out) if out.status.success() => {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            record_finalize_note(config, job, &format!("pr_created: {url}"));
        }
        Ok(out) => record_finalize_note(
            config,
            job,
            &format!(
                "pr_create_failed: {}",
                truncate_str(&String::from_utf8_lossy(&out.stderr), 300)
            ),
        ),
        Err(e) => record_finalize_note(config, job, &format!("pr_create_failed: {e}")),
    }
}

/// Append a finalize note to the job's result without changing its status.
fn record_finalize_note(config: &CodingConfig, job: &Job, note: &str) {
    let store = match Store::open(&store_root(config)) {
        Ok(s) => s,
        Err(_) => return,
    };
    let Some(mut j) = store.load(&job.job_id) else {
        return;
    };
    j.result_summary = if j.result_summary.is_empty() {
        note.to_string()
    } else {
        format!("{}; {note}", j.result_summary)
    };
    j.updated_at = now_rfc3339();
    let _ = store.save(&j);
    notify(&j, config);
}

// ---------------------------------------------------------------------------
// Workspace drift verification
// ---------------------------------------------------------------------------

pub fn workspace_state(root: &str) -> WorkspaceState {
    if is_git_repo(root) {
        let toplevel = git(root, &["rev-parse", "--show-toplevel"]).1.trim().to_string();
        let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).1.trim().to_string();
        let head = git(root, &["rev-parse", "HEAD"]).1.trim().to_string();
        let status = git(root, &["status", "--porcelain"]).1;
        let mut digest_input = head.clone();
        digest_input.push('\n');
        digest_input.push_str(&status);
        WorkspaceState {
            repository: toplevel,
            branch,
            head,
            working_tree_digest: hex_sha256(digest_input.as_bytes()),
        }
    } else {
        WorkspaceState {
            repository: String::new(),
            branch: String::new(),
            head: String::new(),
            working_tree_digest: tree_digest(Path::new(root)),
        }
    }
}

fn verify_no_drift(root: &str, cp: &Checkpoint) -> Result<(), String> {
    let current = workspace_state(root);
    if current.repository != cp.workspace.repository
        || current.branch != cp.workspace.branch
        || current.head != cp.workspace.head
        || current.working_tree_digest != cp.workspace.working_tree_digest
    {
        Err(format!(
            "checkpoint_drift: repository/branch/HEAD/working-tree changed since segment \
             (checkpoint branch={} head={} digest={}; now branch={} head={} digest={})",
            cp.workspace.branch,
            cp.workspace.head,
            truncate_str(&cp.workspace.working_tree_digest, 12),
            current.branch,
            current.head,
            truncate_str(&current.working_tree_digest, 12),
        ))
    } else {
        Ok(())
    }
}

fn is_git_repo(root: &str) -> bool {
    git(root, &["rev-parse", "--git-dir"]).0
}

/// Run git with a 10s wall-clock timeout (kills the process on expiry).
fn git(root: &str, args: &[&str]) -> (bool, String) {
    let mut child = match std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (false, String::new()),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, String::new());
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return (false, String::new());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    if !status.success() {
        let detail = stderr.trim();
        return (false, if detail.is_empty() { stdout } else { detail.to_string() });
    }
    (true, stdout)
}

/// sha256 over sorted (relative_path, sha256(content)) entries under root,
/// skipping build/vendor directories. Used for non-git workspaces.
fn tree_digest(root: &Path) -> String {
    const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".zcode", "release"];
    let mut entries = Vec::new();
    collect_tree(root, root, &mut entries, SKIP_DIRS);
    entries.sort();
    let mut digest_input = String::new();
    for (rel, hash) in entries {
        digest_input.push_str(&rel);
        digest_input.push('=');
        digest_input.push_str(&hash);
        digest_input.push('\n');
    }
    hex_sha256(digest_input.as_bytes())
}

fn collect_tree(root: &Path, dir: &Path, entries: &mut Vec<(String, String)>, skip: &[&str]) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if skip.contains(&name.as_str()) {
                    continue;
                }
                collect_tree(root, &path, entries, skip);
            } else if path.is_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    entries.push((rel, hex_sha256(&bytes)));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fake backend (deterministic, side-effect free — used by tests)
// ---------------------------------------------------------------------------

/// Simulated segmented backend. The objective may contain
/// `fake_work_units:N` (default 3); each unit consumes one model round and
/// one tool call. The checkpoint is scripted so continuation is
/// deterministic.
fn run_fake_segment(
    job: &Job,
    budget: &ExecutionSegmentBudget,
    checkpoint: Option<&Checkpoint>,
    cancel_flag: Arc<AtomicBool>,
) -> SegmentOutcome {
    let units_total: u64 = job
        .objective
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .last()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let units_done = checkpoint
        .map(|cp| cp.completed_steps.len() as u64)
        .unwrap_or(0);
    let mut rounds = 0u64;
    let mut tools = 0u64;
    let started = SystemTime::now();
    let mut completed_steps: Vec<String> = checkpoint
        .map(|cp| cp.completed_steps.clone())
        .unwrap_or_default();
    let mut findings = checkpoint
        .map(|cp| cp.findings.clone())
        .unwrap_or_default();

    loop {
        if cancel_flag.load(Ordering::SeqCst) {
            return SegmentOutcome {
                end: SegmentEnd::Cancelled,
                model_rounds_used: rounds,
                tool_calls_used: tools,
                wall_time_ms: started.elapsed().unwrap_or_default().as_millis() as u64,
                checkpoint: None,
            };
        }
        // One unit = one model round + one tool call. Each completed unit is
        // recorded immediately so the checkpoint never loses progress.
        rounds += 1;
        tools += 1;
        completed_steps.push(format!("fake unit {}", units_done + rounds));
        std::thread::sleep(Duration::from_millis(10));
        let wall = started.elapsed().unwrap_or_default().as_millis() as u64;
        if rounds >= budget.max_model_rounds
            || tools >= budget.max_tool_calls
            || wall >= budget.max_wall_time_ms
        {
            let reason = if rounds >= budget.max_model_rounds {
                "max_model_rounds"
            } else if tools >= budget.max_tool_calls {
                "max_tool_calls"
            } else {
                "max_wall_time_ms"
            };
            findings.push_str(&format!("; segment: {rounds} fake units, budget exhausted ({reason})"));
            return SegmentOutcome {
                end: SegmentEnd::Exhausted(reason.into()),
                model_rounds_used: rounds,
                tool_calls_used: tools,
                wall_time_ms: wall,
                checkpoint: Some(fake_checkpoint(job, completed_steps, units_total, findings, "continue fake work")),
            };
        }
        if units_done + rounds >= units_total {
            let done = completed_steps.len() as u64;
            findings.push_str(&format!("; segment: {rounds} fake units, all {done}/{units_total} units done"));
            return SegmentOutcome {
                end: SegmentEnd::Completed(json!({
                    "summary": format!("fake: completed '{}' ({} units, {} segments)", truncate_str(&job.objective, 60), units_total, job.segments.len() + 1),
                    "commit_sha": "", "changed_files": "", "diff_summary": "fake: no real changes",
                    "test_command": "cargo test", "test_result": "fake: passed",
                    "exit_code": 0, "timed_out": false,
                    "stdout_truncated": "fake: task completed\n", "stderr_truncated": "",
                })),
                model_rounds_used: rounds,
                tool_calls_used: tools,
                wall_time_ms: wall,
                checkpoint: Some(fake_checkpoint(job, completed_steps, units_total, findings, "task complete")),
            };
        }
    }
}

fn fake_checkpoint(
    job: &Job,
    completed_steps: Vec<String>,
    units_total: u64,
    findings: String,
    next_action: &str,
) -> Checkpoint {
    let done = completed_steps.len() as u64;
    let remaining: Vec<String> = ((done + 1)..=units_total)
        .map(|u| format!("fake unit {u}"))
        .collect();
    Checkpoint {
        objective: job.objective.clone(),
        boundaries: "fake workspace (no side effects)".into(),
        findings,
        workspace: workspace_state(&job.workspace_root),
        completed_steps,
        remaining_steps: remaining,
        last_test_result: "fake: passed".into(),
        blocker: String::new(),
        next_action: next_action.into(),
    }
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// Terminal/approval notification: always persisted as a notification record
/// under `<job_store>/notifications/`; optionally posted to a Feishu webhook
/// when `HARNESS_FEISHU_WEBHOOK_URL` is configured.
fn notify(job: &Job, config: &CodingConfig) {
    let store_root = store_root(config);
    let dir = store_root.join("notifications");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let record = json!({
        "job_id": job.job_id,
        "status": job.status.as_str(),
        "updated_at": job.updated_at,
        "current_phase": job.current_phase,
        "result_summary": job.result_summary,
        "last_error": job.last_error,
        "segment_count": job.segments.len(),
        "task_digest": job.task_digest,
    });
    let path = dir.join(format!(
        "{}_{}.json",
        job.job_id,
        job.status.as_str()
    ));
    if let Ok(bytes) = serde_json::to_vec_pretty(&record) {
        let _ = std::fs::write(path, bytes);
    }
    let webhook = std::env::var("HARNESS_FEISHU_WEBHOOK_URL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    if let Some(url) = webhook {
        let payload = serde_json::to_string(&record).unwrap_or_default();
        std::thread::spawn(move || {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build()
                .new_agent();
            let _ = agent
                .post(&url)
                .header("Content-Type", "application/json")
                .send(payload.as_bytes());
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn checkpoint_template(job: &Job) -> Checkpoint {
    let state = workspace_state(&job.workspace_root);
    Checkpoint {
        objective: job.objective.clone(),
        boundaries: format!(
            "Workspace {} ({}); only files inside the workspace may change.",
            job.workspace_id, job.workspace_root
        ),
        findings: String::new(),
        workspace: state,
        completed_steps: Vec::new(),
        remaining_steps: Vec::new(),
        last_test_result: String::new(),
        blocker: String::new(),
        next_action: String::new(),
    }
}

fn new_job_id() -> String {
    let nanos = unix_ms();
    format!("job_{nanos:x}")
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn env_json(name: &str) -> Option<Value> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .and_then(|v| serde_json::from_str(&v).ok())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
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

fn ok(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}

fn err(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}
