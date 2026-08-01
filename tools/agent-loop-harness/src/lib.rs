//! Agent Loop Harness V0 (Bootstrap phase) — same-session auto-continuation.
//!
//! An EXTERNAL process. It talks to the Kernel ONLY through its public HTTP
//! contract (`/v1/events` for reading run outcomes, `/v1/session-continuation`
//! for requesting the next Run in the same session). It does NOT depend on the
//! Kernel crate, does NOT read the Kernel DB, and does NOT know any product
//! concept (no task, progress, checkpoint, Development Job).
//!
//! The `run.outcome.resolve.v0` policy is implemented HERE (inside this
//! Harness) as the V0 default policy:
//!
//! ```text
//! yielded + not-waiting-user + under external limits  → continue_same_session
//! completed / waiting_user                            → reply_and_wait
//! failed / cancelled                                  → stop
//! ```
//!
//! Harness-local state (cursor, processed run ids, automatic-run count, total
//! wall time, consecutive failures) exists ONLY to avoid duplicate work and
//! infinite loops — it is not a task/progress system.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Harness configuration. All limits are LOCAL to this external process —
/// they never enter the Kernel product model.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Kernel base URL, e.g. `http://127.0.0.1:4130`.
    pub kernel_url: String,
    /// Kernel IPC bearer token (same as AGENT_CORE_IPC_TOKEN).
    pub ipc_token: String,
    /// Where the harness-local state file lives (persisted across restarts).
    pub state_path: PathBuf,
    /// `max_automatic_runs_since_user_input` — stop auto-continuing after this
    /// many consecutive automatic Runs since the last real user input.
    pub max_automatic_runs: u64,
    /// `max_total_wall_time_since_user_input` (ms) — total wall-clock budget
    /// for automatic continuation since the last real user input.
    pub max_total_wall_time_ms: u64,
    /// `max_consecutive_failures` — stop after this many consecutive failed
    /// Runs (avoids retrying a permanently broken task forever).
    pub max_consecutive_failures: u64,
    /// Poll interval between `/v1/events` reads.
    pub poll_interval_ms: u64,
    /// The neutral continuation text sent as the next Run's input.
    pub continuation_text: String,
    /// Feishu replay fields, required only when the session channel is Feishu.
    pub feishu_replay: Option<FeishuReplay>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FeishuReplay {
    pub sender_open_id: String,
    pub chat_id: String,
    pub chat_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Harness-local state (persisted; NOT a task system)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HarnessState {
    /// Last event sequence consumed (`cursor` for `/v1/events?cursor=`).
    pub events_cursor: i64,
    /// Run ids already resolved by the policy — prevents re-processing.
    pub processed_run_ids: Vec<String>,
    /// Automatic continuation count since the last real user input.
    pub automatic_run_count: u64,
    /// Total wall-clock time (ms) of automatic continuation since the last
    /// real user input.
    pub total_wall_time_ms: u64,
    /// Consecutive failed outcomes.
    pub consecutive_failures: u64,
    /// Timestamp of the last real (non-continuation) user input, if any.
    pub last_user_input_at: Option<String>,
}

impl HarnessState {
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Outcome derivation from observed events
// ---------------------------------------------------------------------------

/// Generic Run outcome derived from the Kernel's terminal journal events.
/// Mirrors the `run.outcome.resolve.v0` input vocabulary (frozen boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Yielded,
    Completed,
    WaitingUser,
    Failed,
    Cancelled,
}

/// Per-Run observation accumulated from journal events.
#[derive(Debug, Default)]
pub struct RunObservation {
    pub run_id: String,
    pub session_id: Option<String>,
    pub principal_id: Option<String>,
    pub saw_yield_signal: bool,
    pub saw_terminal: bool,
}

impl RunObservation {
    /// Derive the generic outcome from the observed terminal events.
    /// A yield signal (budget exhausted with `exhaustion_action=yield`) makes
    /// the outcome `yielded`; a normal completion without yield is
    /// `completed` (the model finished its turn — treat as waiting for user);
    /// `RunFailed` / `RunBudgetTerminated` make it `failed`.
    pub fn outcome(&self) -> RunOutcome {
        if self.saw_yield_signal {
            RunOutcome::Yielded
        } else if self.saw_terminal {
            RunOutcome::Completed
        } else {
            RunOutcome::WaitingUser
        }
    }
}

// ---------------------------------------------------------------------------
// V0 policy — run.outcome.resolve.v0 semantics (implemented inside Harness)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    ContinueSameSession,
    ReplyAndWait,
    Stop,
}

pub struct PolicyContext<'a> {
    pub outcome: RunOutcome,
    pub state: &'a HarnessState,
    pub config: &'a HarnessConfig,
}

/// V0 default policy (frozen in AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1 §7.4):
///
/// ```text
/// yielded + not-waiting-user + under limits  → continue_same_session
/// completed / waiting_user                   → reply_and_wait
/// failed / cancelled                         → stop
/// ```
pub fn resolve_policy(ctx: &PolicyContext) -> PolicyAction {
    match ctx.outcome {
        RunOutcome::Yielded => {
            let since_user_input_ms = user_input_elapsed_ms(ctx.state);
            let under_run_limit = ctx.state.automatic_run_count < ctx.config.max_automatic_runs;
            let under_wall_limit =
                since_user_input_ms.map(|ms| ms < ctx.config.max_total_wall_time_ms).unwrap_or(true);
            let under_failure_limit =
                ctx.state.consecutive_failures < ctx.config.max_consecutive_failures;
            if under_run_limit && under_wall_limit && under_failure_limit {
                PolicyAction::ContinueSameSession
            } else {
                PolicyAction::Stop
            }
        }
        RunOutcome::Completed | RunOutcome::WaitingUser => PolicyAction::ReplyAndWait,
        RunOutcome::Failed | RunOutcome::Cancelled => PolicyAction::Stop,
    }
}

fn user_input_elapsed_ms(state: &HarnessState) -> Option<u64> {
    let last = state.last_user_input_at.as_ref()?;
    let parsed: DateTime<Utc> = DateTime::parse_from_rfc3339(last).ok()?.with_timezone(&Utc);
    let now = Utc::now();
    if now < parsed {
        return Some(0);
    }
    Some((now - parsed).num_milliseconds().max(0) as u64)
}

// ---------------------------------------------------------------------------
// Kernel HTTP client (narrow contract only)
// ---------------------------------------------------------------------------

pub struct KernelClient {
    pub base_url: String,
    pub ipc_token: String,
}

#[derive(Debug, Deserialize)]
pub struct ObserveResponse {
    pub events: Vec<ObservedEvent>,
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObservedEvent {
    pub event_id: String,
    pub event_kind: String,
    pub occurred_at: String,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub principal_id: Option<String>,
    pub correlation_id: Option<String>,
    pub payload: Value,
}

impl KernelClient {
    pub fn new(base_url: &str, ipc_token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            ipc_token: ipc_token.to_string(),
        }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent()
    }

    /// Pull a page of journal events after `cursor`.
    pub fn observe(&self, cursor: i64, limit: i64) -> Result<ObserveResponse> {
        let url = format!("{}/v1/events?cursor={}&limit={}", self.base_url, cursor, limit);
        let response = Self::agent()
            .get(&url)
            .header("authorization", &format!("Bearer {}", self.ipc_token))
            .call()
            .with_context(|| format!("observe GET failed: {url}"))?;
        let body = response
            .into_body()
            .with_config()
            .limit(8 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| anyhow::anyhow!("observe body read failed: {e}"))?;
        let parsed: ObserveResponse =
            serde_json::from_str(&body).map_err(|e| anyhow::anyhow!("observe parse failed: {e}"))?;
        Ok(parsed)
    }

    /// Request the next Run in the same session. Returns `true` on first
    /// acceptance, `false` when the Kernel reports a duplicate idempotency_key.
    pub fn request_continuation(
        &self,
        session_id: &str,
        trigger_run_id: &str,
        principal_id: Option<&str>,
        continuation_text: &str,
        idempotency_key: &str,
        feishu_replay: Option<&FeishuReplay>,
    ) -> Result<bool> {
        let mut body = json!({
            "session_id": session_id,
            "trigger_run_id": trigger_run_id,
            "principal_id": principal_id.unwrap_or(""),
            "continuation_text": continuation_text,
            "idempotency_key": idempotency_key,
        });
        if let Some(replay) = feishu_replay {
            body["feishu_replay"] = json!({
                "sender_open_id": replay.sender_open_id,
                "chat_id": replay.chat_id,
                "chat_type": replay.chat_type,
            });
        }
        let url = format!("{}/v1/session-continuation", self.base_url);
        let response = Self::agent()
            .post(&url)
            .header("authorization", &format!("Bearer {}", self.ipc_token))
            .header("content-type", "application/json")
            .send_json(body)
            .with_context(|| format!("continuation POST failed: {url}"))?;
        let body = response
            .into_body()
            .with_config()
            .limit(1024 * 1024)
            .read_to_string()
            .map_err(|e| anyhow::anyhow!("continuation body read failed: {e}"))?;
        let parsed: Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("continuation parse failed: {e}"))?;
        if parsed["ok"] != json!(true) {
            bail!("continuation rejected: {}", parsed);
        }
        Ok(parsed["duplicate"] != json!(true))
    }
}

// ---------------------------------------------------------------------------
// Single observation pass (testable)
// ---------------------------------------------------------------------------

/// Process one page of events: update the harness-local state and, when a
/// terminal Run outcome resolves to `continue_same_session`, request the next
/// Run. Returns the number of continuation requests issued.
pub fn run_once(
    client: &KernelClient,
    state: &mut HarnessState,
    config: &HarnessConfig,
) -> Result<u64> {
    let page = client.observe(state.events_cursor, 500)?;
    let mut continuations = 0u64;
    // Per-Run observations keyed by run_id. A run is observed through its
    // RunStarted → terminal events; processed runs are skipped.
    let mut observations: std::collections::HashMap<String, RunObservation> =
        std::collections::HashMap::new();

    for event in &page.events {
        let Some(run_id) = event.run_id.clone() else { continue };
        if state.processed_run_ids.contains(&run_id) {
            continue;
        }
        let obs = observations.entry(run_id.clone()).or_default();
        obs.run_id = run_id.clone();
        obs.session_id = event.session_id.clone().or_else(|| obs.session_id.clone());
        obs.principal_id = event.principal_id.clone().or_else(|| obs.principal_id.clone());

        match event.event_kind.as_str() {
            "ToolBudgetExhausted" | "ToolLoopWallClockExceeded" => {
                if event.payload["exhaustion_action"] == json!("yield") {
                    obs.saw_yield_signal = true;
                }
            }
            "RunFailed" | "RunBudgetTerminated" => {
                obs.saw_terminal = true;
            }
            "RunCompleted" => {
                obs.saw_terminal = true;
            }
            _ => {}
        }
    }
    // Keep the cursor advancing even when runs are already processed: the
    // cursor marks what we READ, processed_run_ids marks what we ACTED on.
    if page.next_cursor > state.events_cursor {
        state.events_cursor = page.next_cursor;
    }

    for (run_id, obs) in observations {
        if state.processed_run_ids.contains(&run_id) {
            continue;
        }
        // Only resolve runs that reached a terminal event.
        if !obs.saw_terminal && !obs.saw_yield_signal {
            continue;
        }
        let Some(session_id) = obs.session_id.clone() else { continue };
        let outcome = obs.outcome();
        let action = resolve_policy(&PolicyContext {
            outcome,
            state,
            config,
        });
        match action {
            PolicyAction::ContinueSameSession => {
                let idempotency_key = format!(
                    "cont:{}:{}",
                    run_id,
                    uuid::Uuid::new_v4().simple()
                );
                let accepted = client.request_continuation(
                    &session_id,
                    &run_id,
                    obs.principal_id.as_deref(),
                    &config.continuation_text,
                    &idempotency_key,
                    config.feishu_replay.as_ref(),
                )?;
                if accepted {
                    state.automatic_run_count += 1;
                    state.total_wall_time_ms += 1;
                    continuations += 1;
                }
            }
            PolicyAction::ReplyAndWait | PolicyAction::Stop => {}
        }
        state.processed_run_ids.push(run_id);
        // Persist after every resolution so a restart never re-processes.
        state.save(&config.state_path)?;
    }
    Ok(continuations)
}

/// Continuous loop: observe → resolve → continue until stopped.
pub fn run_forever(config: HarnessConfig) -> Result<()> {
    let client = KernelClient::new(&config.kernel_url, &config.ipc_token);
    let mut state = HarnessState::load(&config.state_path);
    let poll = Duration::from_millis(config.poll_interval_ms);
    loop {
        match run_once(&client, &mut state, &config) {
            Ok(_) => {}
            Err(error) => {
                eprintln!("agent-loop-harness: observe pass failed: {error}");
            }
        }
        std::thread::sleep(poll);
    }
}

// ---------------------------------------------------------------------------
// Binary entry point
// ---------------------------------------------------------------------------

/// Build the config from environment variables (see README in this crate).
pub fn config_from_env() -> Result<HarnessConfig> {
    let kernel_url = std::env::var("AGENT_LOOP_KERNEL_URL").unwrap_or_else(|_| {
        std::env::var("KERNEL_API_URL").unwrap_or_else(|_| "http://127.0.0.1:4130".to_string())
    });
    let ipc_token = std::env::var("AGENT_LOOP_IPC_TOKEN")
        .or_else(|_| std::env::var("AGENT_CORE_IPC_TOKEN"))
        .map_err(|_| anyhow::anyhow!("AGENT_LOOP_IPC_TOKEN (or AGENT_CORE_IPC_TOKEN) required"))?;
    let state_path = std::env::var("AGENT_LOOP_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let dir = std::env::var("AGENT_CORE_DATA_DIR").unwrap_or_else(|_| {
                format!("{}/.agent-core", std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            });
            PathBuf::from(format!("{dir}/agent-loop-harness/state.json"))
        });
    let max_automatic_runs = parse_env_u64("AGENT_LOOP_MAX_AUTOMATIC_RUNS", 5)?;
    let max_total_wall_time_ms = parse_env_u64("AGENT_LOOP_MAX_TOTAL_WALL_TIME_MS", 600_000)?;
    let max_consecutive_failures = parse_env_u64("AGENT_LOOP_MAX_CONSECUTIVE_FAILURES", 3)?;
    let poll_interval_ms = parse_env_u64("AGENT_LOOP_POLL_INTERVAL_MS", 500)?;
    let continuation_text =
        std::env::var("AGENT_LOOP_CONTINUATION_TEXT").unwrap_or_else(|_| "继续".to_string());
    let feishu_replay = match std::env::var("AGENT_LOOP_FEISHU_SENDER_OPEN_ID") {
        Ok(sender_open_id) => Some(FeishuReplay {
            sender_open_id,
            chat_id: std::env::var("AGENT_LOOP_FEISHU_CHAT_ID")?,
            chat_type: std::env::var("AGENT_LOOP_FEISHU_CHAT_TYPE").ok(),
        }),
        Err(_) => None,
    };
    Ok(HarnessConfig {
        kernel_url,
        ipc_token,
        state_path,
        max_automatic_runs,
        max_total_wall_time_ms,
        max_consecutive_failures,
        poll_interval_ms,
        continuation_text,
        feishu_replay,
    })
}

fn parse_env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| anyhow::anyhow!("{name} must be a number, got {value:?}")),
        Err(_) => Ok(default),
    }
}
