mod cache;
mod contract;

#[cfg(test)]
use self::contract::validate_request_contract;
use self::contract::{validate_contracts, validate_request_source};
use super::model::{self, ModelConfig};
use super::GenerationError;
use agent_core_kernel::domain::{DevelopmentRequest, TargetKind};
use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

const CARGO_TOML: &str =
    include_str!("../../../templates/hook-consumer-service/Cargo.toml.template");
const CARGO_LOCK: &str =
    include_str!("../../../templates/hook-consumer-service/Cargo.lock.template");
const MAIN_RS: &str = include_str!("../../../templates/hook-consumer-service/main.rs.template");
const SUPPORT_RS: &str =
    include_str!("../../../templates/hook-consumer-service/support.rs.template");
const TEST_KIT: &str = "hook-consumer-service-contract-v0";
const ENTRY: &str = "target/release/generated-hook-consumer";
const COMPILE_PROBE_INPUT: &str = r#"{"schema_version":"event.observe.v0","next_cursor":3,"has_more":false,"events":[{"event_id":"completed-1","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-1","payload":{"profile":"default","provider":"test","model":"model-a<img src=x onerror=alert(1)>","latency_ms":20,"input_tokens":10,"cached_input_tokens":2,"output_tokens":5,"reasoning_tokens":1,"total_tokens":16}},{"event_id":"failed-1","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-15T11:00:00Z","run_id":"run-2","payload":{"profile":"analysis","provider":"test","model":"model-b","latency_ms":30,"error_category":"dependency_unavailable"}},{"event_id":"future-1","event_kind":"future.observed.fact.v9","occurred_at":"2026-07-15T12:00:00Z","payload":{"unknown":{"nested":true}}}]}"#;

pub(super) fn generate(
    artifact_root: &Path,
    request: &DevelopmentRequest,
) -> Result<Value, GenerationError> {
    if request.target_kind != TargetKind::HookConsumerService
        || request.build_profile != "hook-consumer-service-v0"
        || request.deployment_profile != "managed-service-v0"
    {
        return Err(GenerationError::new("GENERATOR_NOT_CONFIGURED_FOR_PROFILE"));
    }
    let base = artifact_root.join("generated");
    std::fs::create_dir_all(&base)?;
    let key_hash = hex::encode(Sha256::digest(request.idempotency_key.as_bytes()));
    let candidate_id = format!("generated_hook_{}", &key_hash[..24]);
    let lock_path = base.join(format!("{candidate_id}.lock"));
    let mut lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;

    let candidate_root = base.join(&candidate_id);
    let candidate = candidate_root.join("candidate");
    let result = if candidate.is_dir() {
        load_existing(request, &candidate_id, &candidate)
    } else {
        let config = ModelConfig::from_env()?;
        let (mut source, initial_attempts) = model::generate_module_with_retry(&config, request)?;
        let mut model_calls = initial_attempts;
        let max_repairs = repair_budget(initial_attempts);
        for repair_round in 0..=max_repairs {
            match compile_probe(&base, &candidate_id, request, &source) {
                Ok(()) => break,
                Err(CompileProbeError::Candidate(diagnostics))
                    if repair_round < max_repairs && model_calls < 6 =>
                {
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "generator compile probe failed before repair {}:\n{}",
                        repair_round + 1,
                        diagnostics
                    );
                    let diagnostics =
                        sanitize_model_diagnostics(&diagnostics, &base, &candidate_id);
                    let (repaired, attempts) = model::repair_module_with_retry(
                        &config,
                        request,
                        &source,
                        &diagnostics,
                        6 - model_calls,
                    )?;
                    model_calls += attempts;
                    source = repaired;
                }
                Err(CompileProbeError::Candidate(diagnostics)) => {
                    #[cfg(debug_assertions)]
                    eprintln!("generator probe repair exhausted:\n{diagnostics}");
                    let error_code = if diagnostics.contains("GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED") {
                        "GENERATOR_ACCEPTANCE_REPAIR_EXHAUSTED"
                    } else {
                        "GENERATOR_COMPILE_REPAIR_EXHAUSTED"
                    };
                    return Err(GenerationError::new(error_code));
                }
                Err(CompileProbeError::Infrastructure) => {
                    return Err(GenerationError::new(
                        "GENERATOR_COMPILE_PROBE_INFRASTRUCTURE_FAILURE",
                    ));
                }
            }
        }
        materialize(&base, &candidate_id, request, &source, config.model())
    };
    if let Ok(value) = &result {
        writeln!(lock, "{}", value["candidate_digest"].as_str().unwrap_or(""))?;
        lock.sync_all()?;
    }
    let _ = FileExt::unlock(&lock);
    result
}

fn repair_budget(initial_attempts: usize) -> usize {
    if initial_attempts < 3 {
        4
    } else {
        3
    }
}

enum CompileProbeError {
    Candidate(String),
    Infrastructure,
}

fn compile_probe(
    base: &Path,
    candidate_id: &str,
    request: &DevelopmentRequest,
    source: &str,
) -> Result<(), CompileProbeError> {
    model::validate_generated_source(source).map_err(|_| CompileProbeError::Infrastructure)?;
    validate_request_source(request, source).map_err(CompileProbeError::Candidate)?;
    let probe = base.join(format!(
        ".{candidate_id}.compile-probe.{}.{}",
        std::process::id(),
        unique_suffix()
    ));
    let result = (|| {
        std::fs::create_dir_all(probe.join("src"))
            .map_err(|_| CompileProbeError::Infrastructure)?;
        let runtime = MAIN_RS.replace(
            "__COMPONENT_PRELUDE__",
            &model::component_prelude(source).map_err(|_| CompileProbeError::Infrastructure)?,
        );
        for (path, bytes) in [
            (probe.join("Cargo.toml"), CARGO_TOML.as_bytes()),
            (probe.join("Cargo.lock"), CARGO_LOCK.as_bytes()),
            (probe.join("src/main.rs"), runtime.as_bytes()),
            (probe.join("src/support.rs"), SUPPORT_RS.as_bytes()),
            (probe.join("src/component.rs"), source.as_bytes()),
        ] {
            std::fs::write(path, bytes).map_err(|_| CompileProbeError::Infrastructure)?;
        }

        #[cfg(all(target_os = "macos", debug_assertions))]
        if std::env::var("CODING_GENERATOR_TEST_ALLOW_HOST_COMPILE").as_deref() == Ok("1") {
            let mut command = std::process::Command::new("cargo");
            command
                .args(["build", "--locked"])
                .current_dir(&probe)
                .env_clear();
            for name in ["PATH", "HOME", "TMPDIR", "CARGO_HOME", "RUSTUP_HOME"] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            let output = command
                .output()
                .map_err(|_| CompileProbeError::Infrastructure)?;
            if !output.status.success() {
                return Err(CompileProbeError::Candidate(truncate_diagnostics(
                    &String::from_utf8_lossy(&output.stderr),
                )));
            }
            return host_contract_probe(
                &probe.join("target/debug/generated-hook-consumer"),
                request,
            );
        }

        let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|home| format!("{home}/.cargo"))
                .unwrap_or_default()
        });
        let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|home| format!("{home}/.rustup"))
                .unwrap_or_default()
        });
        let target_dir = probe.join("target").to_string_lossy().to_string();
        let result = crate::hcr::gates::run_command_sandboxed(
            Path::new("/usr/bin/env"),
            &["cargo", "build", "--locked"],
            &probe,
            std::time::Duration::from_secs(60),
            &[],
            &[
                ("CARGO_TARGET_DIR", target_dir.as_str()),
                ("CARGO_HOME", &cargo_home),
                ("RUSTUP_HOME", &rustup_home),
            ],
        )
        .map_err(|_| CompileProbeError::Infrastructure)?;
        if result.timed_out || result.child_cleanup.as_str() != "confirmed" {
            return Err(CompileProbeError::Infrastructure);
        }
        if result.exit_code != 0 {
            return Err(CompileProbeError::Candidate(truncate_diagnostics(
                &result.stderr,
            )));
        }
        let binary = probe.join("target/debug/generated-hook-consumer");
        let contract = crate::hcr::gates::run_command_sandboxed(
            &binary,
            &["--profile-contract-test"],
            &probe,
            std::time::Duration::from_secs(15),
            &[COMPILE_PROBE_INPUT],
            &[],
        )
        .map_err(|_| CompileProbeError::Infrastructure)?;
        if contract.child_cleanup.as_str() != "confirmed" {
            return Err(CompileProbeError::Infrastructure);
        }
        if contract.exit_code != 0 || contract.timed_out {
            return Err(CompileProbeError::Candidate(format!(
                "PROFILE_CONTRACT_TEST_FAILED\nstdout:\n{}\nstderr:\n{}",
                truncate_diagnostics(&contract.stdout),
                truncate_diagnostics(&contract.stderr),
            )));
        }
        validate_contracts(request, &contract.stdout).map_err(CompileProbeError::Candidate)
    })();
    #[cfg(debug_assertions)]
    let keep_probe = std::env::var("CODING_GENERATOR_TEST_KEEP_PROBES").as_deref() == Ok("1");
    #[cfg(not(debug_assertions))]
    let keep_probe = false;
    if keep_probe {
        eprintln!("generator debug probe retained at {}", probe.display());
    } else {
        let _ = std::fs::remove_dir_all(&probe);
    }
    result
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn host_contract_probe(
    binary: &Path,
    request: &DevelopmentRequest,
) -> Result<(), CompileProbeError> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let mut command = std::process::Command::new(binary);
    command
        .arg("--profile-contract-test")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| CompileProbeError::Infrastructure)?;
    let pid = child.id();
    child
        .stdin
        .take()
        .ok_or(CompileProbeError::Infrastructure)?
        .write_all(COMPILE_PROBE_INPUT.as_bytes())
        .map_err(|_| CompileProbeError::Infrastructure)?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    let output = match receiver.recv_timeout(std::time::Duration::from_secs(15)) {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => return Err(CompileProbeError::Infrastructure),
        Err(_) => {
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
            return Err(CompileProbeError::Candidate(
                "PROFILE_CONTRACT_TEST_TIMEOUT".into(),
            ));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(CompileProbeError::Candidate(format!(
            "PROFILE_CONTRACT_TEST_FAILED\nstdout:\n{}\nstderr:\n{}",
            truncate_diagnostics(&stdout),
            truncate_diagnostics(&String::from_utf8_lossy(&output.stderr)),
        )));
    }
    validate_contracts(request, &stdout).map_err(CompileProbeError::Candidate)
}

fn truncate_diagnostics(value: &str) -> String {
    let mut end = value.len().min(16 * 1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn sanitize_model_diagnostics(value: &str, base: &Path, candidate_id: &str) -> String {
    let base = base.to_string_lossy();
    value
        .replace(base.as_ref(), "<generator-root>")
        .replace(candidate_id, "<candidate-id>")
}

fn materialize(
    base: &Path,
    candidate_id: &str,
    request: &DevelopmentRequest,
    source: &str,
    model_name: &str,
) -> Result<Value, GenerationError> {
    model::validate_generated_source(source)?;
    let source_digest = format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())));
    let component_manifest = cache::component_manifest(request, &source_digest, model_name);
    let specification = cache::specification(request);

    let temp = base.join(format!(
        ".{candidate_id}.{}.{}.tmp",
        std::process::id(),
        unique_suffix()
    ));
    let candidate = temp.join("candidate");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(candidate.join("src"))?;
    write_file(&candidate.join("Cargo.toml"), CARGO_TOML.as_bytes())?;
    write_file(&candidate.join("Cargo.lock"), CARGO_LOCK.as_bytes())?;
    let runtime = MAIN_RS.replace("__COMPONENT_PRELUDE__", &model::component_prelude(source)?);
    write_file(&candidate.join("src/main.rs"), runtime.as_bytes())?;
    write_file(&candidate.join("src/support.rs"), SUPPORT_RS.as_bytes())?;
    write_file(&candidate.join("src/component.rs"), source.as_bytes())?;
    write_file(
        &candidate.join("manifest.json"),
        &serde_json::to_vec_pretty(&component_manifest)
            .map_err(|_| GenerationError::new("CANDIDATE_GENERATION_FAILED"))?,
    )?;
    write_file(
        &candidate.join("specification.json"),
        &serde_json::to_vec_pretty(&specification)
            .map_err(|_| GenerationError::new("CANDIDATE_GENERATION_FAILED"))?,
    )?;
    sync_dir(&candidate.join("src"))?;
    sync_dir(&candidate)?;
    sync_dir(&temp)?;
    std::fs::rename(&temp, base.join(candidate_id))?;
    sync_dir(base)?;
    load_existing(
        request,
        candidate_id,
        &base.join(candidate_id).join("candidate"),
    )
}

fn load_existing(
    request: &DevelopmentRequest,
    candidate_id: &str,
    candidate: &Path,
) -> Result<Value, GenerationError> {
    let manifest_bytes = std::fs::read(candidate.join("manifest.json"))?;
    let component_manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| GenerationError::new("CANDIDATE_CACHE_INVALID"))?;
    if component_manifest
        .pointer("/generation/development_request_id")
        .and_then(Value::as_str)
        != Some(request.request_id.as_str())
        || component_manifest
            .get("component_id")
            .and_then(Value::as_str)
            != Some(request.name.as_str())
    {
        return Err(GenerationError::new("CANDIDATE_CACHE_IDENTITY_MISMATCH"));
    }
    let source = std::fs::read_to_string(candidate.join("src/component.rs"))?;
    model::validate_generated_source(&source)?;
    let expected_source_digest =
        format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())));
    if component_manifest
        .pointer("/generation/module_digest")
        .and_then(Value::as_str)
        != Some(expected_source_digest.as_str())
    {
        return Err(GenerationError::new("CANDIDATE_CACHE_INVALID"));
    }
    cache::validate(candidate, request, &source, &component_manifest)?;
    let digest = crate::hcr::candidate::compute_digest(candidate)
        .map_err(|_| GenerationError::new("CANDIDATE_GENERATION_FAILED"))?;
    Ok(json!({
        "candidate_id": candidate_id,
        "candidate_ref": format!("generated/{candidate_id}/candidate"),
        "candidate_digest": digest,
        "request_id": request.request_id,
        "component_manifest": component_manifest,
    }))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), GenerationError> {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_dir(path: &Path) -> Result<(), GenerationError> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests;
