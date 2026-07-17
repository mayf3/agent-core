//! Per-kit Acceptance Bundle digest computation.
//!
//! Each Acceptance Kit has its own bundle manifest computed at build time.
//! The bundle digest includes:
//! - The kit's own source files (public spec + private verifier)
//! - Kit-specific fixtures
//! - The shared verifier engine (shared_verifier_engine.rs)
//! - The verifier runtime version (crate version)
//!
//! Shared engine changes affect all kits that depend on it.
//!
//! Also retains the production safety guard: `test-fixtures` feature must
//! never be enabled in release builds.

use sha2::{Digest, Sha256};
use std::path::Path;

fn main() {
    // --- Production safety guard (unchanged from original) ---
    let has_fixtures = std::env::var("CARGO_FEATURE_TEST_FIXTURES").is_ok();
    if has_fixtures {
        let profile = std::env::var("PROFILE").unwrap_or_default();
        if profile == "release" {
            panic!(
                "FATAL: test-fixtures feature is FORBIDDEN in release builds. \
                 Use `cargo test --features test-fixtures` instead."
            );
        }
        println!(
            "cargo:warning=test-fixtures feature is enabled (profile={}). \
             This is valid only for test contexts.",
            profile
        );
    }

    // --- Per-kit bundle digest computation ---
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let src_dir = Path::new(&manifest_dir).join("src").join("self_evolution").join("acceptance_kit");
    let crate_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();

    // Token Dashboard bundle
    if let Some(digest) = compute_bundle_digest(
        "token-dashboard-v0",
        &src_dir,
        &[
            "token_dashboard/mod.rs",
            "token_dashboard/public_spec.rs",
            "token_dashboard/private_verifier.rs",
            "shared_verifier_engine.rs",
        ],
        &crate_version,
    ) {
        println!("cargo:rustc-env=TOKEN_DASHBOARD_BUNDLE_DIGEST={}", digest);
        println!("cargo:warning=TOKEN_DASHBOARD_BUNDLE_DIGEST={}", &digest[..24]);
    }

    // Failure Event Viewer bundle
    if let Some(digest) = compute_bundle_digest(
        "failure-event-viewer-v0",
        &src_dir,
        &[
            "failure_event_viewer.rs",
            "shared_verifier_engine.rs",
        ],
        &crate_version,
    ) {
        println!("cargo:rustc-env=FAILURE_VIEWER_BUNDLE_DIGEST={}", digest);
        println!("cargo:warning=FAILURE_VIEWER_BUNDLE_DIGEST={}", &digest[..24]);
    }

    // Trigger rebuild when any acceptance_kit file changes
    println!("cargo:rerun-if-changed={}", src_dir.join("shared_verifier_engine.rs").display());
    println!("cargo:rerun-if-changed={}", src_dir.join("token_dashboard").display());
    println!("cargo:rerun-if-changed={}", src_dir.join("failure_event_viewer.rs").display());
}

/// Compute a bundle digest for a kit by building a canonical manifest
/// that lists each asset file with its SHA-256, plus the verifier runtime
/// version. The final digest is SHA-256 of the canonical manifest bytes.
fn compute_bundle_digest(
    kit_id: &str,
    src_dir: &Path,
    asset_paths: &[&str],
    crate_version: &str,
) -> Option<String> {
    let mut canonical = Vec::new();

    // Build a deterministic manifest: kit_id, version, then sorted file entries.
    canonical.extend_from_slice(format!("kit_id:{kit_id}\n").as_bytes());
    canonical.extend_from_slice(format!("verifier_runtime_version:{crate_version}\n").as_bytes());
    canonical.extend_from_slice(b"assets:\n");

    for rel_path in asset_paths {
        let full_path = src_dir.join(rel_path);
        if !full_path.exists() {
            // File might not exist yet (e.g., during incremental build after file split)
            return None;
        }
        let content = std::fs::read(&full_path).ok()?;
        let file_digest = format!("sha256:{}", hex::encode(Sha256::digest(&content)));
        canonical.extend_from_slice(format!("  {rel_path} {file_digest}\n").as_bytes());
    }

    let bundle_digest = format!("bundle_sha256:{}", hex::encode(Sha256::digest(&canonical)));
    Some(bundle_digest)
}
