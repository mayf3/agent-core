//! Deterministic hook-consumer-service fixture.
//! Materializes a candidate from the published templates and a known-good
//! component module, allowing the Five Gates to run without a real model.

use agent_core_kernel::domain::{DevelopmentRequest, TargetKind};
use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

const CARGO_TOML: &str = include_str!("../../templates/hook-consumer-service/Cargo.toml.template");
const CARGO_LOCK: &str = include_str!("../../templates/hook-consumer-service/Cargo.lock.template");
const MAIN_RS: &str = include_str!("../../templates/hook-consumer-service/main.rs.template");
const SUPPORT_RS: &str = include_str!("../../templates/hook-consumer-service/support.rs.template");

/// A known-good component module that satisfies the profile-contract test.
pub(crate) const COMPONENT_RS: &str = r#"use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn initial_state() -> Value {
    json!({
        "daily": {},
        "runs": {},
        "models": {},
        "profiles": {},
        "meta": {
            "total_calls": 0,
            "total_input": 0,
            "total_cached": 0,
            "total_output": 0,
            "total_reasoning": 0,
            "total_latency": 0,
            "total_unavailable": 0,
            "total_failures": 0
        }
    })
}

fn ensure_map(value: &mut Value, path: &[&str]) -> &mut serde_json::Map<String, Value> {
    let mut current = value;
    for part in path {
        if !current.is_object() {
            *current = json!({});
        }
        let map = current.as_object_mut().expect("ensure_map: object");
        if !map.contains_key(*part) {
            map.insert((*part).to_string(), json!({}));
        }
        current = map.get_mut(*part).expect("ensure_map: get_mut");
    }
    current.as_object_mut().expect("ensure_map: final object")
}

fn inc(map: &mut serde_json::Map<String, Value>, key: &str, delta: u64) {
    let entry = map.entry(key.to_string()).or_insert(json!(0));
    let current = entry.as_u64().unwrap_or(0);
    *entry = json!(current + delta);
}

fn extract_tokens(payload: &Value) -> (u64, u64, u64, u64, u64) {
    let input = payload.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let cached = payload.get("cached_input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = payload.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    let reasoning = payload.get("reasoning_tokens").and_then(Value::as_u64).unwrap_or(0);
    let total = payload.get("total_tokens").and_then(Value::as_u64).unwrap_or(0);
    (input, cached, output, reasoning, total)
}

pub fn apply_event(state: &mut Value, event: &Value) {
    let kind = event.get("event_kind").and_then(Value::as_str).unwrap_or("").to_string();
    let occurred_at = event.get("occurred_at").and_then(Value::as_str).unwrap_or("").to_string();
    let date = occurred_at.get(..10).unwrap_or("unknown").to_string();
    let run_id = event.get("run_id").and_then(Value::as_str).unwrap_or("unknown").to_string();
    let payload = event.get("payload");

    let (model, profile) = match payload {
        Some(p) => (
            p.get("model").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            p.get("profile").and_then(Value::as_str).unwrap_or("unknown").to_string(),
        ),
        None => ("unknown".into(), "unknown".into()),
    };

    let (input, cached, output, reasoning, total) = match payload {
        Some(p) => extract_tokens(p),
        None => (0, 0, 0, 0, 0),
    };

    let latency = payload.and_then(|p| p.get("latency_ms").and_then(Value::as_u64)).unwrap_or(0);
    let is_failure = kind == "model.invocation.failed.v0";
    let is_completion = kind == "model.invocation.completed.v0";
    let has_missing = is_completion && (total == 0)
        || payload.map(|p| {
            p.get("input_tokens").and_then(Value::as_u64) == Some(0)
                && p.get("output_tokens").and_then(Value::as_u64) == Some(0)
        }).unwrap_or(false);

    if !is_completion && !is_failure {
        return;
    }

    // Meta counters
    let meta = state.get_mut("meta").and_then(|m| m.as_object_mut());
    if let Some(m) = meta {
        inc(m, "total_calls", 1);
        inc(m, "total_input", input);
        inc(m, "total_cached", cached);
        inc(m, "total_output", output);
        inc(m, "total_reasoning", reasoning);
        inc(m, "total_latency", latency);
        if is_failure { inc(m, "total_failures", 1); }
        if has_missing { inc(m, "total_unavailable", 1); }
    }

    // Daily aggregates
    let day = ensure_map(state, &["daily", &date]);
    inc(day, "calls", 1);
    inc(day, "input", input);
    inc(day, "cached", cached);
    inc(day, "output", output);
    inc(day, "reasoning", reasoning);
    inc(day, "latency", latency);
    if is_failure { inc(day, "failures", 1); }
    if has_missing { inc(day, "unavailable", 1); }

    // Per-run
    let run = ensure_map(state, &["runs", &run_id]);
    inc(run, "calls", 1);
    inc(run, "input", input);
    inc(run, "cached", cached);
    inc(run, "output", output);
    inc(run, "reasoning", reasoning);
    inc(run, "latency", latency);

    // Per-model
    let model_m = ensure_map(state, &["models", &model]);
    inc(model_m, "calls", 1);
    inc(model_m, "input", input);
    inc(model_m, "cached", cached);
    inc(model_m, "output", output);
    inc(model_m, "reasoning", reasoning);
    inc(model_m, "latency", latency);

    // Per-profile
    let profile_m = ensure_map(state, &["profiles", &profile]);
    inc(profile_m, "calls", 1);
    inc(profile_m, "input", input);
    inc(profile_m, "cached", cached);
    inc(profile_m, "output", output);
    inc(profile_m, "reasoning", reasoning);
    inc(profile_m, "latency", latency);
}

pub fn render_json(state: &Value, runtime: &Value) -> Value {
    let today = runtime.get("today_utc").and_then(Value::as_str).unwrap_or("unknown");
    let mut windows = json!({
        "1_day": {"calls":0,"input":0,"cached":0,"output":0,"reasoning":0,"latency":0,"failures":0,"unavailable":0},
        "7_day": {"calls":0,"input":0,"cached":0,"output":0,"reasoning":0,"latency":0,"failures":0,"unavailable":0},
        "30_day": {"calls":0,"input":0,"cached":0,"output":0,"reasoning":0,"latency":0,"failures":0,"unavailable":0}
    });
    if let Some(daily) = state.get("daily").and_then(Value::as_object) {
        for (date_str, day_data) in daily {
            let days_ago = days_between(date_str, today);
            if days_ago <= 1 { accumulate(&mut windows["1_day"], day_data); }
            if days_ago <= 7 { accumulate(&mut windows["7_day"], day_data); }
            if days_ago <= 30 { accumulate(&mut windows["30_day"], day_data); }
        }
    }
    let meta = state.get("meta");
    json!({
        "by_date": state.get("daily"),
        "by_run": state.get("runs"),
        "by_model": state.get("models"),
        "by_profile": state.get("profiles"),
        "windows": windows,
        "telemetry_unavailable": runtime.get("telemetry_unavailable"),
        "last_observed_cursor": runtime.get("last_observed_cursor"),
        "projection_lag": runtime.get("projection_lag"),
        "component_version": runtime.get("component_version"),
        "health": runtime.get("health"),
        "meta": meta,
    })
}

fn days_between(date: &str, today: &str) -> u64 {
    let parse = |s: &str| -> Option<(i64, i64, i64)> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() == 3 {
            let y = parts[0].parse().ok()?;
            let m = parts[1].parse().ok()?;
            let d = parts[2].parse().ok()?;
            Some((y, m, d))
        } else {
            None
        }
    };
    let (Some((y1, m1, d1)), Some((y2, m2, d2))) = (parse(date), parse(today)) else {
        return u64::MAX;
    };
    let days_from_epoch = |y, m, d| -> i64 {
        let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
        365 * y + y / 4 - y / 100 + y / 400 + (153 * m - 457) / 5 + d - 306
    };
    (days_from_epoch(y2, m2, d2) - days_from_epoch(y1, m1, d1)).unsigned_abs()
}

fn accumulate(target: &mut Value, src: &Value) {
    let t = target.as_object_mut();
    let s = src.as_object();
    if let (Some(t), Some(s)) = (t, s) {
        for (k, v) in s {
            if let Some(vu) = v.as_u64() {
                inc(t, k, vu);
            }
        }
    }
}

pub fn render_html(state: &Value, runtime: &Value) -> String {
    let meta = state.get("meta").and_then(|m| m.as_object());
    let total_calls = meta.and_then(|m| m.get("total_calls").and_then(Value::as_u64)).unwrap_or(0);
    let total_input = meta.and_then(|m| m.get("total_input").and_then(Value::as_u64)).unwrap_or(0);
    let total_output = meta.and_then(|m| m.get("total_output").and_then(Value::as_u64)).unwrap_or(0);
    let total_failures = meta.and_then(|m| m.get("total_failures").and_then(Value::as_u64)).unwrap_or(0);
    let avg_latency = if total_calls > 0 {
        meta.and_then(|m| m.get("total_latency").and_then(Value::as_u64)).unwrap_or(0) / total_calls
    } else { 0 };

    let health = runtime.get("health").and_then(Value::as_str).unwrap_or("unknown");
    let version = runtime.get("component_version").and_then(Value::as_str).unwrap_or("unknown");
    let cursor = runtime.get("last_observed_cursor").and_then(Value::as_u64).map(|v| v.to_string()).unwrap_or_else(|| "unknown".into());
    let lag = runtime.get("projection_lag").and_then(Value::as_str).unwrap_or("unknown");
    let unavailable = runtime.get("telemetry_unavailable").and_then(Value::as_bool).unwrap_or(true);

    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Token Dashboard</title>\
         <style>body{{font-family:sans-serif;margin:2em}}table{{border-collapse:collapse}}\
         th,td{{border:1px solid #ccc;padding:0.5em;text-align:right}}th{{background:#f5f5f5}}\
         </style></head><body>\
         <h1>Token Dashboard</h1>\
         <p>Health: {} | Version: {} | Cursor: {} | Lag: {} | Telemetry Unavailable: {}</p>\
         <h2>Summary</h2>\
         <p>Calls: {} | Input: {} | Output: {} | Failures: {} | Avg Latency: {}ms</p>\
         </body></html>",
        html_escape(health),
        html_escape(version),
        html_escape(&cursor),
        html_escape(lag),
        if unavailable { "true" } else { "false" },
        total_calls,
        total_input,
        total_output,
        total_failures,
        avg_latency,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
"#;

pub fn generate(
    artifact_root: &Path,
    request: &DevelopmentRequest,
) -> Result<Value, std::io::Error> {
    if !supports(request) {
        return Err(std::io::Error::other("hook_consumer fixture mismatch"));
    }
    generate_locked(
        artifact_root,
        &request.idempotency_key,
        &request.request_id,
        &request.name,
    )
}

pub(super) fn supports(request: &DevelopmentRequest) -> bool {
    request.target_kind == TargetKind::HookConsumerService
        && request.name == "token-dashboard"
        && request.build_profile == "hook-consumer-service-v0"
        && request.required_contracts == ["event.observe.v0"]
}

fn generate_locked(
    artifact_root: &Path,
    key: &str,
    request_id: &str,
    component_name: &str,
) -> Result<Value, std::io::Error> {
    let key_hash = hex::encode(Sha256::digest(key.as_bytes()));
    let candidate_id = format!("generated_hook_fixture_{}", &key_hash[..24]);
    let base = artifact_root.join("generated");
    std::fs::create_dir_all(&base)?;
    let lock_path = base.join(format!("{candidate_id}.lock"));
    let mut lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive().map_err(std::io::Error::other)?;

    let candidate = base.join(&candidate_id).join("candidate");
    let result = if candidate.is_dir() {
        load_existing(request_id, component_name, &candidate)
    } else {
        materialize(&base, &candidate_id, request_id, component_name)
    };

    if let Ok(value) = &result {
        let digest = value["candidate_digest"].as_str().unwrap_or("");
        writeln!(lock, "{digest}")?;
        lock.sync_all()?;
    }
    let _ = fs2::FileExt::unlock(&lock);
    result
}

fn materialize(
    base: &Path,
    candidate_id: &str,
    request_id: &str,
    component_name: &str,
) -> Result<Value, std::io::Error> {
    let temp = base.join(format!(".{candidate_id}.{}.tmp", std::process::id()));
    let candidate = temp.join("candidate");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(candidate.join("src"))?;

    // Write template files with known-good component module embedded
    let runtime = MAIN_RS.replace(
        "__COMPONENT_PRELUDE__",
        "use crate::support::html_escape;\nuse crate::support::value_string;\nuse crate::support::value_u64;\nuse crate::support::value_display;\nuse crate::support::ensure_object_path;\nuse crate::support::increment_u64;\nuse crate::support::event_date;\nuse crate::support::within_days;\nuse crate::support::today_utc;",
    );
    std::fs::write(candidate.join("Cargo.toml"), CARGO_TOML)?;
    std::fs::write(candidate.join("Cargo.lock"), CARGO_LOCK)?;
    std::fs::write(candidate.join("src/main.rs"), runtime.as_bytes())?;
    std::fs::write(candidate.join("src/support.rs"), SUPPORT_RS)?;
    std::fs::write(candidate.join("src/component.rs"), COMPONENT_RS)?;

    // Write manifest
    let module_digest = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(COMPONENT_RS.as_bytes()))
    );
    // Placeholder artifact digest — the real digest is computed during
    // the Artifact gate after the binary is built.
    let placeholder_digest =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let manifest = serde_json::json!({
        "schema_version": "component-artifact-v1",
        "component_id": component_name,
        "kind": "hook_consumer_service",
        "profile_id": "hook-consumer-service-v0",
        "contract_catalog_version": "contract-catalog-v1",
        "artifact_digest": placeholder_digest,
        "required_contracts": ["event.observe.v0"],
        "requested_permissions": ["journal.observe"],
        "test_kit": "hook-consumer-service-contract-v0",
        "deployment_profile": "managed-service-v0",
        "runtime_profile": "hook-consumer-v1",
        "healthcheck": "HTTP readiness plus projection status",
        "rollback_policy": "retain last-known-good digest and require a terminal rollback receipt",
        "manifest_id": format!("{}-v0-fixture", component_name),
        "harness_id": "hook-consumer-harness",
        "protocol_version": "external-harness-v1",
        "contract": "event.observe.v0",
        "entry": "target/release/generated-hook-consumer",
        "service": {
            "version": "0.1.0",
            "healthcheck_path": "/health",
            "listen_policy": "loopback"
        },
        "generation": {
            "kind": "request-driven-model-module-v0",
            "model": "fixture",
            "module_digest": &module_digest,
            "mutable_surface": ["src/component.rs"],
            "development_request_id": request_id
        }
    });
    let specification = serde_json::json!({
        "development_request_id": request_id,
        "name": component_name,
        "requested_permissions": ["journal.observe"]
    });
    std::fs::write(
        candidate.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::write(
        candidate.join("specification.json"),
        serde_json::to_vec_pretty(&specification)?,
    )?;

    // Sync and rename
    sync_dir(&candidate.join("src"))?;
    sync_dir(&candidate)?;
    sync_dir(&temp)?;
    std::fs::rename(&temp, base.join(candidate_id))?;
    sync_dir(base)?;

    load_existing(
        request_id,
        component_name,
        &base.join(candidate_id).join("candidate"),
    )
}

fn load_existing(
    request_id: &str,
    component_name: &str,
    candidate: &Path,
) -> Result<Value, std::io::Error> {
    let manifest_bytes = std::fs::read(candidate.join("manifest.json"))?;
    let component_manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    if component_manifest
        .pointer("/generation/development_request_id")
        .and_then(Value::as_str)
        != Some(request_id)
        || component_manifest
            .get("component_id")
            .and_then(Value::as_str)
            != Some(component_name)
    {
        return Err(std::io::Error::other("CANDIDATE_CACHE_IDENTITY_MISMATCH"));
    }
    let source = std::fs::read_to_string(candidate.join("src/component.rs"))?;
    let expected_source_digest =
        format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())));
    if component_manifest
        .pointer("/generation/module_digest")
        .and_then(Value::as_str)
        != Some(expected_source_digest.as_str())
    {
        return Err(std::io::Error::other("CANDIDATE_CACHE_INVALID"));
    }
    let digest = crate::hcr::candidate::compute_digest(candidate).map_err(std::io::Error::other)?;
    // The candidate directory is .../<candidate_id>/candidate/. Derive the
    // candidate_id from the parent directory name.
    let candidate_dir = candidate.parent().unwrap_or(candidate);
    let candidate_name = candidate_dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("unknown");
    Ok(json!({
        "candidate_id": candidate_name,
        "candidate_ref": format!("generated/{candidate_name}/candidate"),
        "candidate_digest": digest,
        "request_id": request_id,
        "component_manifest": component_manifest,
    }))
}

fn sync_dir(path: &Path) -> Result<(), std::io::Error> {
    std::fs::File::open(path).and_then(|f| f.sync_all())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::contract_catalog::CONTRACT_CATALOG_VERSION;
    use agent_core_kernel::domain::DevelopmentRequestDraft;

    fn hook_consumer_request() -> DevelopmentRequest {
        let mut draft =
            DevelopmentRequestDraft::new(TargetKind::HookConsumerService, "token-dashboard".into());
        draft.requirements = vec!["token usage dashboard via event.observe.v0".into()];
        draft.required_contracts = vec!["event.observe.v0".into()];
        draft.requested_permissions = vec!["journal.observe".into()];
        draft.acceptance_criteria = vec!["projects token totals from observed events".into()];
        DevelopmentRequest::from_draft(
            draft,
            "principal:test".into(),
            "scope:test".into(),
            "message:test".into(),
            "development:test".into(),
            CONTRACT_CATALOG_VERSION.into(),
        )
        .unwrap()
    }

    fn unique_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "hook_fix_{}_{}_{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn fixture_materializes_hook_consumer_manifest() {
        let root = unique_root("manifest");
        let result = generate(&root, &hook_consumer_request()).unwrap();
        let m = &result["component_manifest"];
        assert_eq!(m["profile_id"], "hook-consumer-service-v0");
        assert_eq!(m["test_kit"], "hook-consumer-service-contract-v0");
        assert_eq!(m["kind"], "hook_consumer_service");
        let p = root.join(result["candidate_ref"].as_str().unwrap());
        for f in &[
            "Cargo.toml",
            "src/main.rs",
            "src/support.rs",
            "src/component.rs",
            "manifest.json",
        ] {
            assert!(p.join(f).exists(), "missing {f}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Same request + same source → same digest across different root dirs.
    #[test]
    fn candidate_digest_is_stable_across_different_directories() {
        let request = hook_consumer_request();
        let root1 = unique_root("stable1");
        let root2 = unique_root("stable2");
        let r1 = generate(&root1, &request).unwrap();
        let r2 = generate(&root2, &request).unwrap();
        assert_eq!(
            r1["candidate_digest"], r2["candidate_digest"],
            "digest must be stable across different artifact roots"
        );
        let _ = std::fs::remove_dir_all(root1);
        let _ = std::fs::remove_dir_all(root2);
    }

    /// Same request + same source → same digest at different times.
    #[test]
    fn candidate_digest_is_stable_across_time() {
        let request = hook_consumer_request();
        let root = unique_root("time");
        let r1 = generate(&root, &request).unwrap();
        // Re-generate into the same root (second call hits cached path).
        let r2 = generate(&root, &request).unwrap();
        assert_eq!(
            r1["candidate_digest"], r2["candidate_digest"],
            "digest must match across cached re-generation"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Different development_message_id → different request_id →
    /// different specification.json content → different digest.
    #[test]
    fn different_request_different_digest() {
        use agent_core_kernel::domain::DevelopmentRequestDraft;
        let draft = |msg: &str| -> DevelopmentRequest {
            let mut d = DevelopmentRequestDraft::new(
                TargetKind::HookConsumerService,
                "token-dashboard".into(),
            );
            d.requirements = vec!["token usage dashboard via event.observe.v0".into()];
            d.required_contracts = vec!["event.observe.v0".into()];
            d.requested_permissions = vec!["journal.observe".into()];
            d.acceptance_criteria = vec!["projects token totals from observed events".into()];
            DevelopmentRequest::from_draft(
                d,
                "principal:test".into(),
                "scope:test".into(),
                "message:test".into(),
                msg.into(),
                CONTRACT_CATALOG_VERSION.into(),
            )
            .unwrap()
        };
        let req1 = draft("development:test-a");
        let req2 = draft("development:test-b");

        let root1 = unique_root("diff1");
        let root2 = unique_root("diff2");
        let r1 = generate(&root1, &req1).unwrap();
        let r2 = generate(&root2, &req2).unwrap();
        assert_ne!(
            r1["candidate_digest"], r2["candidate_digest"],
            "different requests must produce different digests"
        );
        let _ = std::fs::remove_dir_all(root1);
        let _ = std::fs::remove_dir_all(root2);
    }
}
