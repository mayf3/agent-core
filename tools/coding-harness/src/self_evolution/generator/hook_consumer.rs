mod cache;
mod contract;

use self::contract::validate_profile_contract;
use super::model::{self, ModelConfig};
use super::GenerationError;
use crate::self_evolution::acceptance_kit::AcceptanceKitId;
use crate::self_evolution::acceptance_selector;
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

/// Generic compile probe input for runtime profile contract testing.
///
/// Contains only event.observe.v0 envelope fields and generic event types.
/// No model invocation business fields (tokens, latency, model names, etc.).
/// This ensures the generic probe tests only:
/// - Event envelope parsing
/// - Cursor handling
/// - Unknown field tolerance
/// - Stable JSON/HTML output without crashing
const COMPILE_PROBE_INPUT: &str = r#"{"schema_version":"event.observe.v0","next_cursor":2,"has_more":false,"events":[{"event_id":"evt-1","event_kind":"example.event.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-1","payload":{"value":1,"label":"example"}},{"event_id":"future-1","event_kind":"future.observed.fact.v9","occurred_at":"2026-07-15T12:00:00Z","payload":{"unknown":{"nested":true}}}]}"#;

/// Maximum total model calls across all phases (generate, compile repair,
/// acceptance repair). This is a single unified budget.
const TOTAL_MODEL_CALL_BUDGET: usize = 6;

/// Run the compiled binary with the given input and return its stdout.
///
/// Reuses the sandbox execution infrastructure from compile_probe.
/// The binary must be a previously compiled `generated-hook-consumer`
/// binary built with `cargo build --locked`.
fn run_binary_with_input(binary: &Path, input: &str) -> Result<String, String> {
    let result = crate::hcr::gates::run_command_sandboxed(
        binary,
        &["--profile-contract-test"],
        binary.parent().unwrap_or(Path::new("/tmp")),
        std::time::Duration::from_secs(15),
        &[input],
        &[],
    )
    .map_err(|_| "PRIVATE_CASE_INFRASTRUCTURE_FAILURE".to_string())?;
    if result.child_cleanup.as_str() != "confirmed" {
        return Err("PRIVATE_CASE_SANDBOX_FAILURE".to_string());
    }
    if result.exit_code != 0 {
        return Err(format!(
            "PRIVATE_CASE_EXIT_CODE={}\nstdout:\n{}\nstderr:\n{}",
            result.exit_code, result.stdout, result.stderr,
        ));
    }
    Ok(result.stdout)
}

/// Production-equivalent verification entry point.
///
/// Given frozen source bytes and an acceptance kit:
/// 1. Compile the source into a temporary probe binary (once).
/// 2. Run the generic profile probe on the binary.
/// 3. Run each kit private verification case on the same binary.
///
/// This is the same function called by generate() and by
/// known-good/incorrect candidate tests. It returns `Ok(probe_stdout)`
/// when all checks pass, or `Err(diagnostics)` on the first failure.
pub(super) fn verify_frozen_candidate(
    base: &Path,
    candidate_id: &str,
    request: &DevelopmentRequest,
    source: &str,
    kit: AcceptanceKitId,
) -> Result<String, CompileProbeError> {
    // 1. Write a compile probe, build it, and get the binary
    let probe = base.join(format!(
        ".{candidate_id}.compile-probe.{}.{}",
        std::process::id(),
        unique_suffix()
    ));
    let binary = probe.join("target/debug/generated-hook-consumer");
    let result = (|| {
        std::fs::create_dir_all(probe.join("src"))
            .map_err(|_| CompileProbeError::Infrastructure)?;

        // Pre-validate source
        model::validate_generated_source(source).map_err(|_| CompileProbeError::Infrastructure)?;

        // Check source policy
        contract::validate_source(kit.kit_id(), source).map_err(CompileProbeError::Candidate)?;

        // Write probe files
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

        // 2. Compile once (sandboxed)
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
        let build = crate::hcr::gates::run_command_sandboxed(
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
        if build.timed_out || build.child_cleanup.as_str() != "confirmed" {
            return Err(CompileProbeError::Infrastructure);
        }
        if build.exit_code != 0 {
            return Err(CompileProbeError::Candidate(truncate_diagnostics(
                &build.stderr,
            )));
        }

        // 3. Run generic profile contract probe
        let generic = crate::hcr::gates::run_command_sandboxed(
            &binary,
            &["--profile-contract-test"],
            &probe,
            std::time::Duration::from_secs(15),
            &[COMPILE_PROBE_INPUT],
            &[],
        )
        .map_err(|_| CompileProbeError::Infrastructure)?;
        if generic.child_cleanup.as_str() != "confirmed" {
            return Err(CompileProbeError::Infrastructure);
        }
        if generic.exit_code != 0 || generic.timed_out {
            return Err(CompileProbeError::Candidate(format!(
                "PROFILE_CONTRACT_TEST_FAILED\nstdout:\n{}\nstderr:\n{}",
                truncate_diagnostics(&generic.stdout),
                truncate_diagnostics(&generic.stderr),
            )));
        }
        validate_profile_contract(COMPILE_PROBE_INPUT, &generic.stdout)
            .map_err(CompileProbeError::Candidate)?;

        // 4. Run each kit private verification case on the SAME binary
        for case in kit.private_verification_cases() {
            let case_stdout = run_binary_with_input(&binary, case.input).map_err(|e| {
                CompileProbeError::Candidate(format!(
                    "PRIVATE_CASE_FAILURE case={}\n{}",
                    case.case_id, e
                ))
            })?;
            kit.verify(request, source, case.input, &case_stdout)
                .map_err(|diagnostics| {
                    CompileProbeError::Candidate(format!(
                        "PRIVATE_CASE_FAILURE case={}\n{}",
                        case.case_id, diagnostics
                    ))
                })?;
        }

        // All checks passed
        Ok(generic.stdout)
    })();

    // Clean up probe directory
    #[cfg(debug_assertions)]
    let keep_probe = std::env::var("CODING_GENERATOR_TEST_KEEP_PROBES").as_deref() == Ok("1");
    #[cfg(not(debug_assertions))]
    let keep_probe = false;
    if !keep_probe {
        let _ = std::fs::remove_dir_all(&probe);
    }

    result
}

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

    // 1. Resolve acceptance bundle via external selector
    let selection = acceptance_selector::select(request)
        .map_err(|_| GenerationError::new("ACCEPTANCE_KIT_SELECTION_REQUIRED"))?;
    let kit = AcceptanceKitId::resolve(&selection.bundle_ref)
        .map_err(|_| GenerationError::new("ACCEPTANCE_KIT_SELECTION_REQUIRED"))?;

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
        // For cache validation we create a selection from the manifest's stored values
        let manifest_bytes = std::fs::read(candidate.join("manifest.json")).ok();
        let stored_selection = manifest_bytes
            .as_ref()
            .and_then(|bytes| {
                serde_json::from_slice::<Value>(bytes).ok().and_then(|v| {
                    let bundle_ref = v.get("acceptance_bundle_ref")?.as_str()?.to_string();
                    let bundle_digest = v.get("acceptance_bundle_digest")?.as_str()?.to_string();
                    Some(acceptance_selector::AcceptanceSelection::new(
                        &bundle_ref,
                        &bundle_digest,
                    ))
                })
            })
            .unwrap_or(acceptance_selector::AcceptanceSelection::new("", ""));
        load_existing(request, &candidate_id, &candidate, &stored_selection)
    } else {
        let config = ModelConfig::from_env()?;
        let (mut source, initial_attempts) = model::generate_module_with_retry(&config, request)?;
        let mut model_calls = initial_attempts;

        // 2. Unified freeze-verify loop: compile + generic probe + private cases
        loop {
            match verify_frozen_candidate(&base, &candidate_id, request, &source, kit) {
                Ok(_compile_stdout) => break,
                Err(CompileProbeError::Candidate(diagnostics))
                    if model_calls < TOTAL_MODEL_CALL_BUDGET =>
                {
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "generator candidate verification failed before repair {}:\n{}",
                        model_calls, diagnostics
                    );
                    let diagnostics =
                        sanitize_model_diagnostics(&diagnostics, &base, &candidate_id);
                    let (repaired, attempts) = model::repair_module_with_retry(
                        &config,
                        request,
                        &source,
                        &diagnostics,
                        TOTAL_MODEL_CALL_BUDGET - model_calls,
                    )?;
                    model_calls += attempts;
                    source = repaired;
                }
                Err(CompileProbeError::Candidate(diagnostics)) => {
                    #[cfg(debug_assertions)]
                    eprintln!("generator candidate verification exhausted:\n{diagnostics}");
                    return Err(GenerationError::new("GENERATOR_COMPILE_REPAIR_EXHAUSTED"));
                }
                Err(CompileProbeError::Infrastructure) => {
                    return Err(GenerationError::new(
                        "GENERATOR_COMPILE_PROBE_INFRASTRUCTURE_FAILURE",
                    ));
                }
            }
        }

        // 3. Freeze candidate bytes and compute future digest
        let source_digest = format!("sha256:{}", hex::encode(Sha256::digest(source.as_bytes())));
        let component_manifest =
            cache::component_manifest(request, &source_digest, config.model(), &selection);

        // 4. All checks passed — materialize exact frozen bytes
        materialize(
            &base,
            &candidate_id,
            request,
            &source,
            config.model(),
            &component_manifest,
        )
    };
    if let Ok(value) = &result {
        writeln!(lock, "{}", value["candidate_digest"].as_str().unwrap_or(""))?;
        lock.sync_all()?;
    }
    let _ = FileExt::unlock(&lock);
    result
}

enum CompileProbeError {
    Candidate(String),
    Infrastructure,
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
    _model_name: &str,
    component_manifest: &Value,
) -> Result<Value, GenerationError> {
    model::validate_generated_source(source)?;
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
        &serde_json::to_vec_pretty(component_manifest)
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
        &acceptance_selector::AcceptanceSelection::new(
            component_manifest
                .get("acceptance_bundle_ref")
                .and_then(Value::as_str)
                .unwrap_or(""),
            component_manifest
                .get("acceptance_bundle_digest")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
    )
}

fn load_existing(
    request: &DevelopmentRequest,
    candidate_id: &str,
    candidate: &Path,
    selection: &acceptance_selector::AcceptanceSelection,
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
    cache::validate(candidate, request, &source, &component_manifest, selection)?;
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
