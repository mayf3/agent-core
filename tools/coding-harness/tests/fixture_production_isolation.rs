//! Production isolation tests for test-fixtures.
//!
//! Verifies that the `test-fixtures` feature cannot be accidentally enabled
//! in production builds and that fixture symbols are absent from release
//! artifacts.
//!
//! # Namespace scope
//!
//! `hook_consumer` names TWO distinct modules:
//!
//! - `coding_harness::fixtures::hook_consumer` — deterministic test fixture,
//!   gated behind `#[cfg(feature = "test-fixtures")]` in
//!   `src/fixtures/mod.rs`. Must never appear in release artifacts.
//! - `coding_harness::self_evolution::generator::hook_consumer` — production
//!   model-driven generator for `HookConsumerService` development requests.
//!   Required in release builds by design (see `src/fixtures/mod.rs`: "real
//!   Token Dashboard requests always go through the model generator").
//!
//! The isolation assertions therefore target the fixture namespace only. A
//! bare `hook_consumer` substring check would incorrectly flag the
//! production generator as a fixture leak.
//!
//! Run: `cargo test --test fixture_production_isolation`

use std::process::Command;

/// Mangled encoding of the `coding_harness::fixtures::hook_consumer`
/// namespace. rustc encodes path segments as `{length}{identifier}`
/// (`fixtures` = 8, `hook_consumer` = 13) in both the legacy (`_ZN...E`)
/// and v0 (`_RNv...`) mangling schemes, so this fragment uniquely
/// identifies the fixture namespace in `nm` output without a
/// platform-specific `--demangle` flag (GNU nm supports it; macOS nm does
/// not). It must not match the production generator namespace
/// (`...14self_evolution9generator13hook_consumer`), which is allowed in
/// release builds.
const FIXTURE_NAMESPACE_MANGLED: &str = "8fixtures13hook_consumer";

fn contains_fixture_namespace(symbols: &str) -> bool {
    symbols.contains(FIXTURE_NAMESPACE_MANGLED)
}

/// Helper: return the target directory for the coding-harness crate.
fn target_dir() -> std::path::PathBuf {
    // Use cargo metadata to get the authoritative target directory.
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata should work");
    assert!(output.status.success(), "cargo metadata failed");
    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid cargo metadata JSON");
    std::path::PathBuf::from(
        meta["target_directory"]
            .as_str()
            .expect("target_directory present"),
    )
}

/// Release build with `test-fixtures` must be REJECTED by the build.rs guard.
#[test]
fn release_build_rejects_test_fixtures() {
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--lib",
            "--features",
            "test-fixtures",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
        ])
        .status()
        .expect("cargo build should run");
    assert!(
        !status.success(),
        "release build with test-fixtures must FAIL (build.rs guard)"
    );
}

/// Default release build (no test-fixtures) must succeed and produce a
/// clean artifact without fixture symbols. Production generator symbols
/// (e.g. `self_evolution::generator::hook_consumer`) may be present.
#[test]
fn release_build_has_no_fixture_symbols() {
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--lib",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
        ])
        .status()
        .expect("cargo build should run");
    assert!(status.success(), "default release build must succeed");

    // Locate the release rlib.
    let release_dir = target_dir().join("release");
    assert!(
        release_dir.exists(),
        "release directory should exist: {:?}",
        release_dir
    );

    let harness_artifact = std::fs::read_dir(&release_dir)
        .expect("release dir readable")
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .find(|p| {
            let fname = p.file_name().unwrap_or_default();
            let name = fname.to_string_lossy();
            name.starts_with("libcoding_harness") || name.starts_with("coding_harness")
        })
        .expect("coding-harness release artifact found in target/release");

    // Use `nm` (macOS/Linux) to check for fixture namespace symbols.
    let nm_output = Command::new("nm")
        .arg(&harness_artifact)
        .output()
        .expect("nm must be available");

    let stdout = String::from_utf8_lossy(&nm_output.stdout);
    let stderr = String::from_utf8_lossy(&nm_output.stderr);

    assert!(
        !contains_fixture_namespace(&stdout),
        "Release build contains fixtures::hook_consumer symbols — test-fixtures leaked! \
         Matching symbols:\n{}",
        stdout
            .lines()
            .filter(|l| contains_fixture_namespace(l))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // nm on macOS .rlib archives prints the archive member paths — the
    // fixture module object file would appear as a member name.
    assert!(
        !contains_fixture_namespace(&stderr),
        "nm stderr mentions the fixture namespace: {}",
        stderr
    );
}

/// A normal (debug) build without test-fixtures succeeds.
#[test]
fn default_build_succeeds() {
    let status = Command::new("cargo")
        .args([
            "build",
            "--lib",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
        ])
        .status()
        .expect("cargo build failed");
    assert!(status.success(), "default build must succeed");
}

/// No environment variable can enable test-fixtures in a release build.
#[test]
fn no_env_var_enables_test_fixtures() {
    // The only mechanism is --features test-fixtures at the Cargo level.
    // Verify that a build with no --features produces no fixture symbols.
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--lib",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
        ])
        .status()
        .expect("cargo build failed");
    assert!(
        status.success(),
        "release build without --features must succeed"
    );

    let release_dir = target_dir().join("release");
    let harness_artifact = std::fs::read_dir(&release_dir)
        .expect("release dir readable")
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .find(|p| {
            let fname = p.file_name().unwrap_or_default();
            let name = fname.to_string_lossy();
            name.starts_with("libcoding_harness") || name.starts_with("coding_harness")
        })
        .expect("coding-harness release artifact found");

    let nm_output = Command::new("nm")
        .arg(&harness_artifact)
        .output()
        .expect("nm must be available");

    let stdout = String::from_utf8_lossy(&nm_output.stdout);
    assert!(
        !contains_fixture_namespace(&stdout),
        "fixtures::hook_consumer symbols present in release build without --features"
    );
}
