//! Integration tests for the scaffold-context-harness.sh script.
//!
//! Tests that the script creates the expected file structure,
//! validates harness IDs, refuses to overwrite existing directories,
//! and that the generated Harness passes its own test suite.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// ── Helpers ──

/// Unique temp directory name for a test.
fn unique_temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sc_test_{}_{}_{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

/// Path to the scaffold script, resolved relative to the crate root.
fn script_path() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .join("scripts")
        .join("scaffold-context-harness.sh")
}

/// Run the scaffold script with the given args and return (exit_status, stdout, stderr).
fn run_scaffold(args: &[&str]) -> (std::process::ExitStatus, String, String) {
    run_scaffold_in(args, None)
}

/// Run the scaffold script with an optional `current_dir`.
fn run_scaffold_in(
    args: &[&str],
    cwd: Option<&Path>,
) -> (std::process::ExitStatus, String, String) {
    let mut cmd = Command::new(script_path());
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd.output().expect("failed to execute scaffold script");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    (output.status, stdout, stderr)
}

// ── Pre-existing tests ──

#[test]
fn scaffold_creates_expected_files() {
    let root = unique_temp_dir("expected");

    let (status, stdout, stderr) =
        run_scaffold(&["--root", root.to_str().unwrap(), "my-test-harness"]);
    assert!(
        status.success(),
        "scaffold failed:\nstdout:{stdout}\nstderr:{stderr}"
    );

    let project_dir = root.join("my-test-harness");

    // Verify all expected files exist.
    assert!(project_dir.join("README.md").exists(), "README.md missing");
    assert!(
        project_dir.join("package.json").exists(),
        "package.json missing"
    );
    assert!(
        project_dir.join("server.mjs").exists(),
        "server.mjs missing"
    );
    assert!(
        project_dir.join("harness.manifest.json").exists(),
        "harness.manifest.json missing"
    );
    assert!(
        project_dir.join("test").join("server.test.mjs").exists(),
        "test/server.test.mjs missing"
    );

    // Verify harness.manifest.json contains the correct harness_id.
    let manifest_content = std::fs::read_to_string(project_dir.join("harness.manifest.json"))
        .expect("failed to read harness.manifest.json");
    assert!(
        manifest_content.contains(r#""harness_id": "my-test-harness""#),
        "manifest should contain harness_id 'my-test-harness'"
    );

    // Verify manifest kind.
    assert!(
        manifest_content.contains(r#""kind": "context.prepare.v0""#),
        "manifest should contain kind 'context.prepare.v0'"
    );

    // Verify smoke word present in server.mjs.
    let server_content =
        std::fs::read_to_string(project_dir.join("server.mjs")).expect("failed to read server.mjs");
    assert!(
        server_content.contains("EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya"),
        "server.mjs should contain the smoke word"
    );

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_rejects_invalid_harness_id() {
    let root = unique_temp_dir("invalid");
    std::fs::create_dir_all(&root).unwrap();

    // Uppercase ID should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&["--root", root.to_str().unwrap(), "INVALID_ID"]);
    assert!(!status.success(), "uppercase ID should be rejected");
    assert!(
        stderr.contains("invalid harness_id"),
        "expected invalid harness_id error"
    );

    // ID with spaces should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&["--root", root.to_str().unwrap(), "my harness"]);
    assert!(!status.success(), "ID with spaces should be rejected");
    assert!(
        stderr.contains("invalid harness_id"),
        "expected invalid harness_id error"
    );

    // ID with slashes should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&["--root", root.to_str().unwrap(), "a/b"]);
    assert!(!status.success(), "ID with slash should be rejected");
    assert!(
        stderr.contains("invalid harness_id"),
        "expected invalid harness_id error"
    );

    // Empty ID should be rejected.
    let (status, _stdout, _stderr) = run_scaffold(&["--root", root.to_str().unwrap(), ""]);
    assert!(!status.success(), "empty ID should be rejected");

    // ID starting with hyphen should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&["--root", root.to_str().unwrap(), "-bad"]);
    assert!(
        !status.success(),
        "ID starting with hyphen should be rejected"
    );
    assert!(
        stderr.contains("invalid harness_id"),
        "expected invalid harness_id error"
    );

    // ID ending with hyphen should be rejected.
    let (status, _stdout, stderr) = run_scaffold(&["--root", root.to_str().unwrap(), "bad-"]);
    assert!(
        !status.success(),
        "ID ending with hyphen should be rejected"
    );
    assert!(
        stderr.contains("invalid harness_id"),
        "expected invalid harness_id error"
    );

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_refuses_to_overwrite_existing_non_empty_dir() {
    let root = unique_temp_dir("overwrite");
    let target = root.join("existing-harness");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("existing-file.txt"), b"i exist").unwrap();

    let (status, _stdout, stderr) =
        run_scaffold(&["--root", root.to_str().unwrap(), "existing-harness"]);
    assert!(
        !status.success(),
        "should refuse to overwrite non-empty dir"
    );
    assert!(
        stderr.contains("already exists"),
        "expected 'already exists' error"
    );
    assert!(stderr.contains("not empty"), "expected 'not empty' error");

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn generated_harness_tests_pass() {
    let root = unique_temp_dir("npm_test");

    let (status, stdout, stderr) =
        run_scaffold(&["--root", root.to_str().unwrap(), "test-harness-smoke"]);
    assert!(
        status.success(),
        "scaffold failed:\nstdout:{stdout}\nstderr:{stderr}"
    );

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

// ── New hardening tests ──

#[test]
fn scaffold_rejects_symlink_target_dir() {
    let root = unique_temp_dir("symlink");

    // Create a directory for the symlink target and a symlink where the harness would go.
    let real_dir = root.join("real-target");
    let symlink_target = root.join("evil-harness");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, &symlink_target).unwrap();

    let (status, _stdout, stderr) =
        run_scaffold(&["--root", root.to_str().unwrap(), "evil-harness"]);
    assert!(!status.success(), "symlink target should be rejected");
    assert!(stderr.contains("symlink"), "expected 'symlink' error");

    // Clean up.
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_fails_on_unwritable_root_or_target_parent() {
    let root = unique_temp_dir("unwritable");
    std::fs::create_dir_all(&root).unwrap();

    // Make root directory read-only (remove all write bits).
    let original_mode = {
        let meta = std::fs::metadata(&root).unwrap();
        meta.permissions().mode()
    };
    {
        let mut perms = std::fs::metadata(&root).unwrap().permissions();
        // Keep read + execute for owner; remove write bits.
        perms.set_mode(original_mode & 0o555);
        std::fs::set_permissions(&root, perms).unwrap();
    }

    let (status, _stdout, stderr) =
        run_scaffold(&["--root", root.to_str().unwrap(), "test-harness"]);
    assert!(!status.success(), "should fail on unwritable root");
    assert!(
        stderr.contains("not writable"),
        "expected 'not writable' error"
    );

    // Restore permissions so we can clean up.
    {
        let mut perms = std::fs::metadata(&root).unwrap().permissions();
        perms.set_mode(original_mode | 0o200);
        std::fs::set_permissions(&root, perms).unwrap();
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_handles_relative_root_safely() {
    // Run the scaffold inside a temp directory using a relative --root path.
    // The script should resolve the relative path against its command CWD.
    let tmp = unique_temp_dir("rel_root");
    std::fs::create_dir_all(&tmp).unwrap();

    let (status, stdout, stderr) =
        run_scaffold_in(&["--root", "my-root", "rel-harness"], Some(&tmp));
    assert!(
        status.success(),
        "relative root should be handled safely:\nstdout:{stdout}\nstderr:{stderr}"
    );

    // Files should be at tmp/my-root/rel-harness/ after resolution.
    let project_dir = tmp.join("my-root").join("rel-harness");
    assert!(
        project_dir.join("harness.manifest.json").exists(),
        "harness.manifest.json not found at {project_dir:?}"
    );
    assert!(
        project_dir.join("server.mjs").exists(),
        "server.mjs not found at {project_dir:?}"
    );

    // Clean up.
    let _ = std::fs::remove_dir_all(&tmp);
}
