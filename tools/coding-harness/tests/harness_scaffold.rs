//! Integration tests for the scaffold-context-harness.sh script.
//!
//! Tests that the script creates the expected file structure,
//! validates harness IDs, refuses to overwrite existing directories,
//! and that the generated Harness passes its own test suite.

use std::path::PathBuf;
use std::process::Command;

// ── Helpers ──

/// Path to the scaffold script, resolved relative to the crate root.
fn script_path() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir.join("scripts").join("scaffold-context-harness.sh")
}

/// Run the scaffold script with the given args and return (exit_status, stdout, stderr).
fn run_scaffold(args: &[&str]) -> (std::process::ExitStatus, String, String) {
    let output = Command::new(script_path())
        .args(args)
        .output()
        .expect("failed to execute scaffold script");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    (output.status, stdout, stderr)
}

// ── Tests ──

#[test]
fn scaffold_creates_expected_files() {
    let root = std::env::temp_dir().join(format!(
        "sc_test_expected_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let (status, stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "my-test-harness",
    ]);
    assert!(status.success(), "scaffold failed:\nstdout:{stdout}\nstderr:{stderr}");

    let project_dir = root.join("my-test-harness");

    // Verify all expected files exist.
    assert!(project_dir.join("README.md").exists(), "README.md missing");
    assert!(project_dir.join("package.json").exists(), "package.json missing");
    assert!(project_dir.join("server.mjs").exists(), "server.mjs missing");
    assert!(project_dir.join("harness.manifest.json").exists(), "harness.manifest.json missing");
    assert!(project_dir.join("test").join("server.test.mjs").exists(), "test/server.test.mjs missing");

    // Verify harness.manifest.json contains the correct harness_id.
    let manifest_content = std::fs::read_to_string(project_dir.join("harness.manifest.json"))
        .expect("failed to read harness.manifest.json");
    assert!(manifest_content.contains(r#""harness_id": "my-test-harness""#),
        "manifest should contain harness_id 'my-test-harness'");

    // Verify manifest kind.
    assert!(manifest_content.contains(r#""kind": "context.prepare.v0""#),
        "manifest should contain kind 'context.prepare.v0'");

    // Verify smoke word present in server.mjs.
    let server_content = std::fs::read_to_string(project_dir.join("server.mjs"))
        .expect("failed to read server.mjs");
    assert!(server_content.contains("EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya"),
        "server.mjs should contain the smoke word");

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_rejects_invalid_harness_id() {
    let root = std::env::temp_dir().join(format!(
        "sc_test_invalid_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();

    // Uppercase ID should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "INVALID_ID",
    ]);
    assert!(!status.success(), "uppercase ID should be rejected");
    assert!(stderr.contains("invalid harness_id"), "expected invalid harness_id error");

    // ID with spaces should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "my harness",
    ]);
    assert!(!status.success(), "ID with spaces should be rejected");
    assert!(stderr.contains("invalid harness_id"), "expected invalid harness_id error");

    // ID with slashes should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "a/b",
    ]);
    assert!(!status.success(), "ID with slash should be rejected");
    assert!(stderr.contains("invalid harness_id"), "expected invalid harness_id error");

    // Empty ID should be rejected.
    let (status, _stdout, _stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "",
    ]);
    assert!(!status.success(), "empty ID should be rejected");

    // ID starting with hyphen should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "-bad",
    ]);
    assert!(!status.success(), "ID starting with hyphen should be rejected");
    assert!(stderr.contains("invalid harness_id"), "expected invalid harness_id error");

    // ID ending with hyphen should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "bad-",
    ]);
    assert!(!status.success(), "ID ending with hyphen should be rejected");
    assert!(stderr.contains("invalid harness_id"), "expected invalid harness_id error");

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_refuses_to_overwrite_existing_non_empty_dir() {
    let root = std::env::temp_dir().join(format!(
        "sc_test_overwrite_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let target = root.join("existing-harness");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("existing-file.txt"), b"i exist").unwrap();

    let (status, _stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "existing-harness",
    ]);
    assert!(!status.success(), "should refuse to overwrite non-empty dir");
    assert!(stderr.contains("already exists"), "expected 'already exists' error");
    assert!(stderr.contains("not empty"), "expected 'not empty' error");

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn generated_harness_tests_pass() {
    let root = std::env::temp_dir().join(format!(
        "sc_test_npm_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let (status, stdout, stderr) = run_scaffold(&[
        "--root",
        root.to_str().unwrap(),
        "test-harness-smoke",
    ]);
    assert!(status.success(), "scaffold failed:\nstdout:{stdout}\nstderr:{stderr}");

    // Run npm test in the generated directory.
    let project_dir = root.join("test-harness-smoke");
    let npm_output = Command::new("npm")
        .args(["test"])
        .current_dir(&project_dir)
        .output()
        .expect("failed to run npm test in generated harness");

    let npm_stdout = String::from_utf8_lossy(&npm_output.stdout);
    let npm_stderr = String::from_utf8_lossy(&npm_output.stderr);

    assert!(
        npm_output.status.success(),
        "npm test failed in generated harness:\nstdout:{npm_stdout}\nstderr:{npm_stderr}"
    );

    // Verify all 5 tests passed.
    assert!(
        npm_stdout.contains("tests 5"),
        "expected 5 tests, got:\n{npm_stdout}"
    );
    assert!(
        npm_stdout.contains("pass 5"),
        "expected 5 passes, got:\n{npm_stdout}"
    );

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}
