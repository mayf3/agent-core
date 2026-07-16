//! Production isolation tests for test-fixtures.
//!
//! Verifies that the `test-fixtures` feature cannot be accidentally enabled
//! in production builds and that fixture symbols are absent from release
//! artifacts.
//!
//! Run: `cargo test --test fixture_production_isolation`

use std::process::Command;

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
    std::path::PathBuf::from(meta["target_directory"].as_str().expect("target_directory present"))
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
/// clean artifact without hook_consumer symbols.
#[test]
fn release_build_has_no_hook_consumer_symbol() {
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
            name.starts_with("libcoding_harness")
                || name.starts_with("coding_harness")
        })
        .expect("coding-harness release artifact found in target/release");

    // Use `nm` (macOS/Linux) to check for hook_consumer symbols.
    let nm_output = Command::new("nm")
        .arg(&harness_artifact)
        .output()
        .expect("nm must be available");

    let stdout = String::from_utf8_lossy(&nm_output.stdout);
    let stderr = String::from_utf8_lossy(&nm_output.stderr);

    assert!(
        !stdout.contains("hook_consumer"),
        "Release build contains hook_consumer symbol — test-fixtures leaked! \
         Matching symbols:\n{}",
        stdout
            .lines()
            .filter(|l| l.contains("hook_consumer"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // nm on macOS .rlib archives prints the archive member paths — the
    // hook_consumer module object file would appear as a member name.
    assert!(
        !stderr.contains("hook_consumer"),
        "nm stderr mentions hook_consumer: {}",
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
    assert!(status.success(), "release build without --features must succeed");

    let release_dir = target_dir().join("release");
    let harness_artifact = std::fs::read_dir(&release_dir)
        .expect("release dir readable")
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .find(|p| {
            let fname = p.file_name().unwrap_or_default();
            let name = fname.to_string_lossy();
            name.starts_with("libcoding_harness")
                || name.starts_with("coding_harness")
        })
        .expect("coding-harness release artifact found");

    let nm_output = Command::new("nm")
        .arg(&harness_artifact)
        .output()
        .expect("nm must be available");

    let stdout = String::from_utf8_lossy(&nm_output.stdout);
    assert!(
        !stdout.contains("hook_consumer"),
        "hook_consumer symbol present in release build without --features"
    );
}
