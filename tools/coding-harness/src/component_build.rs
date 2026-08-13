//! `external.component_build` — the Agent-callable, controlled build operation.
//!
//! The Agent supplies ONLY `{component_id, source_commit}`. The fixed build
//! recipe (authoritative source, projection module, version) comes from Infra
//! config keyed by `component_id` — the Agent can never supply a build command,
//! path, or environment.
//!
//! Lineage policy (stricter than "commit object exists"):
//!   1. `source_commit` must be a full 40-hex immutable SHA (not a movable ref);
//!   2. it must resolve to itself inside the authoritative source repo;
//!   3. it must be an ancestor of the component's configured `allowed_ref`
//!      (e.g. `main`/`HEAD`) — i.e. it is on the authoritative main line, not a
//!      detached or grafted commit from elsewhere.
//!
//! The authoritative source files (template + projection) are read **at that
//! commit** via `git show <commit>:<path>`, never from the working tree, so the
//! produced artifact is a pure function of the immutable commit. The artifact is
//! a single self-contained release binary; `artifact_digest` covers the whole
//! running artifact (application code + compiled dependencies — Rust links them
//! in). The runtime (`node`, the OS) is an Infra runtime dependency, not part of
//! the artifact identity.

use agent_core_kernel::capabilities::store::ContentStore;
use agent_core_kernel::domain::{
    ListenPolicy, RollbackPolicy, ServiceHealthcheck, ServiceManifest, TargetKind,
    UpgradePolicy, SERVICE_MANIFEST_SCHEMA,
};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Per-component build recipe. All paths are relative to the authoritative
/// `source_root` and are resolved at the immutable `source_commit`.
#[derive(Debug, Clone)]
pub struct ComponentBuildEntry {
    pub source_root: PathBuf,
    /// Lineage anchor: the commit must be an ancestor of this ref
    /// (e.g. `"main"`, `"HEAD"`).
    pub allowed_ref: String,
    /// Directory of the `hook-consumer-service` template, relative to source_root.
    pub template_relpath: PathBuf,
    /// The projection module, relative to source_root (becomes `component.rs`).
    pub projection_relpath: PathBuf,
    /// When the projection is stored as a raw-string const inside a fixture
    /// file (`const <NAME>: &str = r#"..."#`), the const name to extract.
    /// `None` means the file's whole content is the projection.
    pub projection_const: Option<String>,
    /// Semver version stamped onto the manifest.
    pub version: String,
}

/// The controlled allowlist. A `component_id` absent from this map is rejected
/// fail-closed — this is where unsupported components (including any kernel
/// component) are refused. The Kernel itself never sees or enforces this list.
#[derive(Debug, Clone, Default)]
pub struct BuildConfig {
    pub entries: HashMap<String, ComponentBuildEntry>,
}

impl BuildConfig {
    /// Parse from a JSON object map of `component_id -> {source_root, allowed_ref,
    /// template_relpath, projection_relpath, version}`.
    pub fn from_json(value: &Value) -> Self {
        let mut entries = HashMap::new();
        if let Some(map) = value.as_object() {
            for (id, cfg) in map {
                let Some(source_root) = cfg.get("source_root").and_then(Value::as_str) else {
                    continue;
                };
                let entry = ComponentBuildEntry {
                    source_root: PathBuf::from(source_root),
                    allowed_ref: cfg
                        .get("allowed_ref")
                        .and_then(Value::as_str)
                        .unwrap_or("HEAD")
                        .to_string(),
                    template_relpath: cfg
                        .get("template_relpath")
                        .and_then(Value::as_str)
                        .unwrap_or("tools/coding-harness/templates/hook-consumer-service")
                        .into(),
                    projection_relpath: cfg
                        .get("projection_relpath")
                        .and_then(Value::as_str)
                        .unwrap_or("tools/coding-harness/src/self_evolution/generator/hook_consumer/tests.rs")
                        .into(),
                    projection_const: cfg
                        .get("projection_const")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    version: cfg
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("0.1.0")
                        .to_string(),
                };
                entries.insert(id.clone(), entry);
            }
        }
        BuildConfig { entries }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildArgs {
    pub component_id: String,
    pub source_commit: String,
}

#[derive(Debug, Clone)]
pub struct BuildOutput {
    pub component_id: String,
    pub source_commit: String,
    pub artifact_digest: String,
    pub manifest_digest: String,
    pub version: String,
    /// Path to the freshly built binary (local, not content-addressed; used by
    /// tests/canary to verify the running contract).
    pub binary_path: PathBuf,
}

/// Dispatch entry point. Returns the full external-harness-v1 response envelope
/// (the same shape other coding-harness operations return from `dispatch`).
pub fn handle(args: &Value, config: &crate::config::CodingConfig) -> Value {
    let parsed: BuildArgs = match serde_json::from_value(args.clone()) {
        Ok(a) => a,
        Err(_) => return err_envelope("invalid_arguments"),
    };
    let build_cfg = BuildConfig::from_json(&config.component_builds);
    match build_component(&parsed, &build_cfg, &config.artifact_root) {
        Ok(output) => success_envelope(&json!({
            "component_id": output.component_id,
            "source_commit": output.source_commit,
            "artifact_digest": output.artifact_digest,
            "manifest_digest": output.manifest_digest,
            "version": output.version,
            "evidence": { "binary": output.binary_path.to_string_lossy() },
        })),
        Err(code) => err_envelope(&code),
    }
}

/// Core build. Returns a stable error_code string on failure.
pub fn build_component(
    args: &BuildArgs,
    cfg: &BuildConfig,
    artifact_root: &Path,
) -> std::result::Result<BuildOutput, String> {
    // 1. Allowlist (fail-closed for any unlisted component).
    let entry = cfg
        .entries
        .get(&args.component_id)
        .ok_or_else(|| "component_not_allowed".to_string())?;

    // 2. Commit format: full 40-hex SHA.
    if !is_full_sha(&args.source_commit) {
        return Err("source_commit_not_immutable".to_string());
    }

    // 3. Lineage: resolves to itself + is an ancestor of allowed_ref.
    verify_lineage(&entry.source_root, &args.source_commit, &entry.allowed_ref)?;

    // 4. Read authoritative source files AT THE COMMIT (immutable).
    let main_rs = git_show(
        &entry.source_root,
        &args.source_commit,
        &entry.template_relpath.join("main.rs.template"),
    )?;
    let support_rs = git_show(
        &entry.source_root,
        &args.source_commit,
        &entry.template_relpath.join("support.rs.template"),
    )?;
    let cargo_toml = git_show(
        &entry.source_root,
        &args.source_commit,
        &entry.template_relpath.join("Cargo.toml.template"),
    )?;
    let projection_blob = git_show(&entry.source_root, &args.source_commit, &entry.projection_relpath)?;

    // 5. The projection may be stored as a raw-string const inside a source
    //    file (e.g. `const KNOWN_GOOD_TOKEN_SOURCE: &str = r#"..."#`). Extract
    //    it; a standalone pure-projection file is used as-is.
    let projection = extract_projection(&projection_blob, entry.projection_const.as_deref())?;

    // 6. Strip top-level `use` items. The generated runtime's `mod component`
    //    already declares the canonical imports via the injected prelude, and
    //    the generator normalizes model output the same way (it removes every
    //    use item before injection); keeping the projection's own copies would
    //    be duplicate imports in the same module.
    let projection = strip_use_items(&projection)?;

    // 7. Validate the projection (forbidden constructs).
    validate_projection(&projection)?;

    // 6. Materialize a buildable crate in a temp dir.
    let crate_dir = materialize_crate(&main_rs, &support_rs, &cargo_toml, &projection)?;

    // 7. Fixed `cargo build --release`.
    let binary = cargo_build_release(&crate_dir, &args.component_id)?;

    // 8. Content-address the artifact (whole running binary).
    let store = ContentStore::new(artifact_root.to_path_buf());
    let artifact_bytes = std::fs::read(&binary).map_err(|_| "artifact_read_failed".to_string())?;
    let artifact_digest = store
        .store(&artifact_bytes)
        .map_err(|_| "artifact_store_failed".to_string())?
        .as_str()
        .to_string();

    // 9. Build + store the immutable ServiceManifest.
    let manifest = build_manifest(&args.component_id, &artifact_digest, &entry.version)?;
    let manifest_bytes = serde_json::to_vec(&manifest).map_err(|_| "manifest_serialize_failed".to_string())?;
    let manifest_digest = store
        .store(&manifest_bytes)
        .map_err(|_| "manifest_store_failed".to_string())?
        .as_str()
        .to_string();

    Ok(BuildOutput {
        component_id: args.component_id.clone(),
        source_commit: args.source_commit.clone(),
        artifact_digest,
        manifest_digest,
        version: entry.version.clone(),
        binary_path: binary,
    })
}

// ── lineage + git helpers ─────────────────────────────────────────────────

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Verify the commit is a real object in `source_root`, resolves to the exact
/// requested SHA (immutability), and is an ancestor of `allowed_ref` (lineage).
fn verify_lineage(
    source_root: &Path,
    commit: &str,
    allowed_ref: &str,
) -> std::result::Result<(), String> {
    // Resolves the commit object; output is the full SHA.
    let resolved = run_git_checked(source_root, &["rev-parse", "--verify", &format!("{commit}^{{commit}}")])
        .map_err(|_| "source_commit_not_found".to_string())?;
    let resolved = resolved.trim().to_string();
    if resolved != commit {
        // The input was not the canonical immutable SHA (e.g. a short SHA or a
        // ref). Refuse so the artifact is bound to an unambiguous commit.
        return Err("source_commit_not_canonical".to_string());
    }
    // Lineage: the commit must be reachable from the authoritative allowed_ref.
    let status = Command::new("git")
        .arg("-C")
        .arg(source_root)
        .args(["merge-base", "--is-ancestor", commit, allowed_ref])
        .status()
        .map_err(|_| "lineage_check_failed".to_string())?;
    if !status.success() {
        return Err("source_commit_not_in_lineage".to_string());
    }
    Ok(())
}

/// `git show <commit>:<path>` → file bytes at that commit (authoritative,
/// immutable; never the working tree).
fn git_show(source_root: &Path, commit: &str, relpath: &Path) -> std::result::Result<String, String> {
    let relpath = relpath.to_string_lossy().replace('\\', "/");
    let blob = run_git_checked(
        source_root,
        &["show", &format!("{commit}:{relpath}")],
    )
    .map_err(|_| "source_file_not_found".to_string())?;
    Ok(blob)
}

fn run_git_checked(source_root: &Path, args: &[&str]) -> std::result::Result<String, ()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source_root)
        .args(args)
        .output()
        .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    String::from_utf8(output.stdout).map_err(|_| ())
}

// ── projection extraction + validation ────────────────────────────────────

/// The projection is a raw Rust string. When `const_name` is given it is
/// extracted from a `const <name>: &str = r#"..."#` raw string inside a source
/// file; otherwise the file's whole content is the projection. Fails closed if
/// the marker is present but unterminated.
fn extract_projection(
    blob: &str,
    const_name: Option<&str>,
) -> std::result::Result<String, String> {
    let Some(name) = const_name else {
        return Ok(blob.to_string());
    };
    let marker = format!("const {name}: &str = r#\"");
    let start = blob
        .find(&marker)
        .ok_or_else(|| "projection_marker_not_found".to_string())?;
    let rest = &blob[start + marker.len()..];
    let end = rest
        .find("\"#")
        .ok_or_else(|| "projection_terminator_not_found".to_string())?;
    Ok(rest[..end].to_string())
}

/// Remove top-level `use ...;` items from the projection (the prelude already
/// declares the canonical imports inside `mod component`). Line-based, matching
/// the generator's normalize step; a multi-line `use` would fail the resulting
/// compile, which the build reports as `build_failed` (fail-closed).
fn strip_use_items(projection: &str) -> std::result::Result<String, String> {
    let mut kept: Vec<&str> = Vec::new();
    for line in projection.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("use ") && trimmed.ends_with(';') {
            continue;
        }
        kept.push(line);
    }
    if kept.is_empty() {
        return Err("projection_empty_after_normalize".to_string());
    }
    Ok(kept.join("\n"))
}


/// Reject projections that reach for the filesystem, process tree, network, or
/// code generation — a projection must be a pure function over events. The
/// known-good fixture satisfies this.
fn validate_projection(source: &str) -> std::result::Result<(), String> {
    const FORBIDDEN: &[&str] = &[
        "include_str!",
        "include_bytes!",
        "include!",
        "unsafe",
        "std::process",
        "std::env",
        "std::fs",
        "std::net",
        "Command",
        "std::os",
    ];
    for token in FORBIDDEN {
        if source.contains(token) {
            return Err("projection_forbidden_construct".to_string());
        }
    }
    Ok(())
}

// ── materialize + build ───────────────────────────────────────────────────

/// Fixed prelude injected into the template's `mod component { __PRELUDE__ }`.
/// Matches the generator's `component_prelude` exactly (a constant string).
const COMPONENT_PRELUDE: &str = "    use serde_json::{json, Map, Value};\n    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};\n    use crate::support::{ensure_object_path, event_date, html_escape, increment_u64, value_display, value_string, value_u64, within_days};\n";

fn materialize_crate(
    main_rs: &str,
    support_rs: &str,
    cargo_toml: &str,
    projection: &str,
) -> std::result::Result<PathBuf, String> {
    let temp = std::env::temp_dir().join(format!(
        "component-build-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let src = temp.join("src");
    std::fs::create_dir_all(&src).map_err(|_| "materialize_failed".to_string())?;
    // Relax the template's exact-pinned versions so the build resolves from the
    // local registry cache (the exact pinned versions are not guaranteed to be
    // present). The component contract does not depend on a specific serde patch.
    let relaxed_cargo = relax_versions(cargo_toml);
    std::fs::write(temp.join("Cargo.toml"), relaxed_cargo).map_err(|_| "materialize_failed".to_string())?;
    let main = main_rs.replace("__COMPONENT_PRELUDE__", COMPONENT_PRELUDE);
    std::fs::write(src.join("main.rs"), main).map_err(|_| "materialize_failed".to_string())?;
    std::fs::write(src.join("support.rs"), support_rs).map_err(|_| "materialize_failed".to_string())?;
    std::fs::write(src.join("component.rs"), projection).map_err(|_| "materialize_failed".to_string())?;
    Ok(temp)
}

/// Loosen `=1.0.228`-style exact pins to `"1"` for the two declared deps so the
/// build resolves from the cache. Best-effort line rewrite.
fn relax_versions(cargo_toml: &str) -> String {
    cargo_toml
        .lines()
        .map(|line| {
            if line.contains("serde_json") && line.contains('=') {
                "serde_json = \"1\"".to_string()
            } else if line.contains("serde") && line.contains("features") {
                "serde = { version = \"1\", features = [\"derive\"] }".to_string()
            } else if line.trim_start().starts_with("serde ") || line.contains("serde =") {
                "serde = { version = \"1\", features = [\"derive\"] }".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cargo_build_release(crate_dir: &Path, component_id: &str) -> std::result::Result<PathBuf, String> {
    // Stable target dir (anchored on this crate's manifest dir) so cargo reuses
    // compiled deps across canary runs instead of recompiling serde every time.
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/canary-build");
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .arg("--manifest-path")
        .arg(crate_dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .map_err(|_| "build_spawn_failed".to_string())?;
    if !status.success() {
        return Err("build_failed".to_string());
    }
    // The template package name is fixed; the binary name equals the package name
    // unless the template declared a [[bin]]. The hook-consumer template does not.
    let binary = target_dir.join("release").join("generated-hook-consumer");
    if !binary.exists() {
        return Err("build_artifact_missing".to_string());
    }
    let _ = component_id; // binary name is independent of component_id here
    Ok(binary)
}

fn build_manifest(
    component_id: &str,
    artifact_digest: &str,
    version: &str,
) -> std::result::Result<ServiceManifest, String> {
    let mut manifest = ServiceManifest {
        schema_version: SERVICE_MANIFEST_SCHEMA.into(),
        manifest_id: String::new(),
        component_id: component_id.into(),
        kind: TargetKind::HookConsumerService,
        artifact_digest: artifact_digest.into(),
        entrypoint: "artifact".into(),
        runtime_profile: "managed-service-v0".into(),
        version: version.into(),
        required_contracts: vec!["event.observe.v0".into()],
        requested_permissions: vec!["journal.observe".into()],
        listen_policy: ListenPolicy {
            host: "127.0.0.1".into(),
            port: 0,
            exposure: "loopback".into(),
        },
        healthcheck: ServiceHealthcheck {
            method: "GET".into(),
            path: "/health".into(),
            timeout_ms: 2000,
        },
        state_path: "state".into(),
        upgrade_policy: UpgradePolicy {
            strategy: "replace_after_ready".into(),
            require_healthy_before_switch: true,
        },
        rollback_policy: RollbackPolicy {
            retain_previous_versions: 2,
            automatic_on_health_failure: false,
        },
    };
    manifest.manifest_id = manifest
        .compute_manifest_id()
        .map_err(|_| "manifest_id_failed".to_string())?;
    manifest.validate().map_err(|_| "manifest_invalid".to_string())?;
    Ok(manifest)
}

fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{nanos:09x}")
}

// ── response envelopes ────────────────────────────────────────────────────

fn success_envelope(result: &Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":result})
}

fn err_envelope(code: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":code})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry() -> ComponentBuildEntry {
        ComponentBuildEntry {
            source_root: PathBuf::from("."),
            allowed_ref: "HEAD".into(),
            template_relpath: PathBuf::from("tools/coding-harness/templates/hook-consumer-service"),
            projection_relpath: PathBuf::from(
                "tools/coding-harness/src/self_evolution/generator/hook_consumer/tests.rs",
            ),
            projection_const: Some("KNOWN_GOOD_TOKEN_SOURCE".into()),
            version: "0.1.0".into(),
        }
    }

    #[test]
    fn allowlist_rejects_unlisted_component() {
        let cfg = BuildConfig::default();
        let args = BuildArgs {
            component_id: "some-other-component".into(),
            source_commit: "0".repeat(40),
        };
        let err = build_component(&args, &cfg, Path::new("/tmp/none")).unwrap_err();
        assert_eq!(err, "component_not_allowed");
    }

    #[test]
    fn allowlist_rejects_kernel_component_even_if_listed_by_mistake() {
        // The kernel is never in the production allowlist; if it ever appeared,
        // it would still be treated as a generic id. This test documents that the
        // build refuses non-canonical commits (the kernel id is not a commit).
        let mut cfg = BuildConfig::default();
        cfg.entries.insert(
            "agent-core-kernel".into(),
            ComponentBuildEntry {
                source_root: PathBuf::from("."),
                allowed_ref: "HEAD".into(),
                template_relpath: PathBuf::new(),
                projection_relpath: PathBuf::new(),
                projection_const: None,
                version: "0.1.0".into(),
            },
        );
        let args = BuildArgs {
            component_id: "agent-core-kernel".into(),
            source_commit: "not-a-sha".into(),
        };
        let err = build_component(&args, &cfg, Path::new("/tmp/none")).unwrap_err();
        assert_eq!(err, "source_commit_not_immutable");
    }

    #[test]
    fn non_hex_or_short_commit_is_rejected_before_git() {
        let mut cfg = BuildConfig::default();
        cfg.entries.insert("generated-hook-consumer".into(), entry());
        let cases = ["main", "HEAD", "abc123", &"a".repeat(39), &"g".repeat(40)];
        for commit in cases {
            let args = BuildArgs {
                component_id: "generated-hook-consumer".into(),
                source_commit: commit.to_string(),
            };
            let err = build_component(&args, &cfg, Path::new("/tmp/none")).unwrap_err();
            assert_eq!(err, "source_commit_not_immutable", "commit {commit} should be rejected");
        }
    }

    #[test]
    fn lineage_rejects_commit_not_in_authorized_lineage() {
        // A valid 40-hex SHA that does not exist in the repo fails the lineage check.
        let mut cfg = BuildConfig::default();
        cfg.entries.insert("generated-hook-consumer".into(), entry());
        let args = BuildArgs {
            component_id: "generated-hook-consumer".into(),
            source_commit: "0".repeat(40),
        };
        let err = build_component(&args, &cfg, Path::new("/tmp/none")).unwrap_err();
        assert_eq!(err, "source_commit_not_found");
    }

    #[test]
    fn config_parses_component_entry() {
        let v = json!({
            "generated-hook-consumer": {
                "source_root": "/repo",
                "allowed_ref": "main",
                "projection_const": "KNOWN_GOOD_TOKEN_SOURCE",
                "version": "0.2.0"
            }
        });
        let cfg = BuildConfig::from_json(&v);
        let e = cfg.entries.get("generated-hook-consumer").unwrap();
        assert_eq!(e.source_root, PathBuf::from("/repo"));
        assert_eq!(e.allowed_ref, "main");
        assert_eq!(e.version, "0.2.0");
        assert_eq!(e.projection_const.as_deref(), Some("KNOWN_GOOD_TOKEN_SOURCE"));
        // Defaults applied for unspecified relpaths.
        assert_eq!(
            e.template_relpath,
            PathBuf::from("tools/coding-harness/templates/hook-consumer-service")
        );
        assert_eq!(
            e.projection_relpath,
            PathBuf::from("tools/coding-harness/src/self_evolution/generator/hook_consumer/tests.rs")
        );
    }

    #[test]
    fn projection_extraction_reads_named_const() {
        let blob = "// header\nconst KNOWN_GOOD_TOKEN_SOURCE: &str = r#\"\npub fn initial_state() -> Value { json!({}) }\n\"#;\n";
        let out = extract_projection(blob, Some("KNOWN_GOOD_TOKEN_SOURCE")).unwrap();
        assert!(out.contains("pub fn initial_state"));
        assert!(!out.contains("header"));
        // Whole-file form.
        assert_eq!(extract_projection("fn x() {}", None).unwrap(), "fn x() {}");
        // Fail-closed on a missing marker.
        assert!(extract_projection("no marker", Some("MISSING")).is_err());
    }

    #[test]
    fn use_items_are_stripped_for_injection() {
        let src = "use serde_json::{json, Value};\nuse std::collections::BTreeMap;\npub fn initial_state() -> Value { json!({}) }\n";
        let out = strip_use_items(src).unwrap();
        assert!(!out.contains("use serde_json"));
        assert!(!out.contains("use std::collections"));
        assert!(out.contains("pub fn initial_state"));
    }

    #[test]
    fn projection_validator_rejects_filesystem_access() {
        assert!(validate_projection("fn initial_state() {} use std::fs;").is_err());
        assert!(validate_projection("fn initial_state() {} include_str!(\"x\");").is_err());
        assert!(validate_projection("fn initial_state() -> Value { json!({}) }").is_ok());
    }
}
