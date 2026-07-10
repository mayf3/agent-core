//! Integration tests for the smoke-context-harness.mjs runner.
//!
//! Tests that the runner validates manifests, performs health and
//! endpoint checks, and produces correct PASS/FAIL text reports. All tests
//! use temp directories and do not write to ~/.agent-core/harnesses/.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

// ── Helpers ──

/// Unique temp directory name for a test.
fn unique_temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sr_test_{}_{}_{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

/// Path to the scaffold script.
fn scaffold_script_path() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .join("scripts")
        .join("scaffold-context-harness.sh")
}

/// Path to the smoke runner.
fn smoke_runner_path() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir.join("scripts").join("smoke-context-harness.mjs")
}

/// Run the scaffold script with the given args.
fn run_scaffold(args: &[&str]) -> (ExitStatus, String, String) {
    let output = Command::new(scaffold_script_path())
        .args(args)
        .output()
        .expect("failed to execute scaffold script");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status, stdout, stderr)
}

/// Run the smoke runner with the given args and return (exit_status, stdout, stderr).
fn run_smoke_runner(args: &[&str]) -> (ExitStatus, String, String) {
    let output = Command::new("node")
        .arg(smoke_runner_path())
        .args(args)
        .output()
        .expect("failed to execute smoke runner");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status, stdout, stderr)
}

/// Allocate an ephemeral port by binding then immediately releasing.
fn allocate_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Wait for a TCP port to be ready (accepting connections).
fn wait_for_port(host: &str, port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect((host, port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Replace port in a scaffold-generated manifest to use a test-specific port.
fn patch_manifest_port(manifest_path: &Path, old_port: &str, new_port: u16) {
    let content =
        std::fs::read_to_string(manifest_path).expect("failed to read manifest for patching");
    let patched = content.replace(&format!(":{old_port}/"), &format!(":{new_port}/"));
    std::fs::write(manifest_path, patched).expect("failed to write patched manifest");
}

/// Override smoke.command to a simple no-op to avoid hanging npm test.
fn patch_smoke_command(manifest_path: &Path, command: &str) {
    let content =
        std::fs::read_to_string(manifest_path).expect("failed to read manifest for patching");
    let patched = content.replace(
        r#""command": "npm test""#,
        &format!(r#""command": "{command}""#),
    );
    std::fs::write(manifest_path, patched).expect("failed to write patched manifest");
}

/// A managed harness server that kills the child on drop.
struct HarnessServer {
    child: Child,
    #[allow(dead_code)]
    port: u16,
}

impl HarnessServer {
    fn start(project_dir: &Path, port: u16) -> Self {
        let mut child = Command::new("node")
            .arg("server.mjs")
            .current_dir(project_dir)
            .env("PORT", port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start harness server");

        let ready = wait_for_port("127.0.0.1", port, Duration::from_secs(8));
        if !ready {
            let _ = child.kill();
            let _ = child.wait();
            panic!("harness server failed to start on port {port} within 8 seconds");
        }

        HarnessServer { child, port }
    }
}

impl Drop for HarnessServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Assert that the text report indicates PASS (includes READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION).
fn assert_smoke_pass(stdout: &str, stderr: &str) {
    assert!(
        stdout.contains("HARNESS_SMOKE_RUNNER_REPORT"),
        "expected HARNESS_SMOKE_RUNNER_REPORT header:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(
        stdout.contains("READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION"),
        "expected READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION:\nstdout:{stdout}\nstderr:{stderr}"
    );
}

/// Assert that the text report indicates FAIL.
fn assert_smoke_fail(stdout: &str, stderr: &str) {
    assert!(
        stdout.contains("HARNESS_SMOKE_RUNNER_REPORT"),
        "expected HARNESS_SMOKE_RUNNER_REPORT header:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(
        stdout.contains("Result: FAIL"),
        "expected Result: FAIL:\nstdout:{stdout}\nstderr:{stderr}"
    );
}

/// Assert that a specific check label appears as the failed check.
fn assert_failed_check(stdout: &str, check_label: &str) {
    assert!(
        stdout.contains(&format!("Failed check: {check_label}")),
        "expected Failed check: {check_label}, got:\n{stdout}"
    );
}

// ── Tests ──

#[test]
fn smoke_runner_passes_generated_scaffold_harness() {
    let root = unique_temp_dir("pass");
    let port = allocate_port();

    // Scaffold a harness.
    let (status, stdout, stderr) =
        run_scaffold(&["--root", root.to_str().unwrap(), "smoke-test-pass"]);
    assert!(
        status.success(),
        "scaffold failed:\nstdout:{stdout}\nstderr:{stderr}"
    );

    let project_dir = root.join("smoke-test-pass");
    let manifest_path = project_dir.join("harness.manifest.json");

    // Patch manifest URLs to use the allocated port and smoke.command to echo.
    patch_manifest_port(&manifest_path, "17400", port);
    patch_smoke_command(&manifest_path, "echo smoke-pass");

    // Start the harness server.
    let _server = HarnessServer::start(&project_dir, port);

    // Run the smoke runner.
    let (status, stdout, stderr) = run_smoke_runner(&[
        "--manifest",
        manifest_path.to_str().unwrap(),
        "--expect-fragment",
        "EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya",
    ]);

    assert!(
        status.success(),
        "smoke runner should pass:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert_smoke_pass(&stdout, &stderr);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn smoke_runner_rejects_non_local_endpoint() {
    let tmp = unique_temp_dir("nonlocal");
    std::fs::create_dir_all(&tmp).unwrap();
    let manifest_path = tmp.join("harness.manifest.json");

    // Write a manifest with a non-local endpoint (0.0.0.0).
    let manifest = r#"{
  "schema_version": "harness-manifest-v0",
  "harness_id": "test-non-local",
  "kind": "context.prepare.v0",
  "entrypoint": { "command": "echo skip", "cwd": "." },
  "health": { "url": "http://127.0.0.1:17400/health", "expected_status": 200 },
  "endpoint": { "url": "http://0.0.0.0:17400/context.prepare.v0", "local_only": true },
  "permissions": { "read_paths": [], "network": ["0.0.0.0"] },
  "smoke": { "command": "echo ok" },
  "rollback": { "strategy": "disable_hook" }
}"#;
    std::fs::write(&manifest_path, manifest).unwrap();

    let (status, stdout, stderr) =
        run_smoke_runner(&["--manifest", manifest_path.to_str().unwrap()]);

    assert!(
        !status.success(),
        "should reject non-local endpoint:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert_smoke_fail(&stdout, &stderr);
    assert_failed_check(&stdout, "local-only endpoint");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn smoke_runner_fails_when_health_unreachable() {
    let root = unique_temp_dir("no_health");

    // Scaffold a harness but DO NOT start the server.
    let (status, stdout, stderr) =
        run_scaffold(&["--root", root.to_str().unwrap(), "smoke-no-health"]);
    assert!(
        status.success(),
        "scaffold failed:\nstdout:{stdout}\nstderr:{stderr}"
    );

    let project_dir = root.join("smoke-no-health");
    let manifest_path = project_dir.join("harness.manifest.json");

    // Use a port where nothing is listening.
    let dead_port = 18999;

    // Patch manifest to use the dead port.
    patch_manifest_port(&manifest_path, "17400", dead_port);

    // Run smoke runner (server not running).
    let (status, stdout, stderr) =
        run_smoke_runner(&["--manifest", manifest_path.to_str().unwrap()]);

    assert!(
        !status.success(),
        "should fail with no server:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert_smoke_fail(&stdout, &stderr);
    assert_failed_check(&stdout, "health");

    // Failure output must include disclaimer confirming no env modification.
    assert!(
        stdout.contains("No Kernel env was modified."),
        "failure report must include 'No Kernel env was modified.':\n{stdout}"
    );
    assert!(
        stdout.contains("No hook was enabled."),
        "failure report must include 'No hook was enabled.':\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn smoke_runner_rejects_invalid_manifest_shape() {
    let tmp = unique_temp_dir("bad_manifest");
    std::fs::create_dir_all(&tmp).unwrap();
    let path = |name: &str| tmp.join(name);

    // Case 1: invalid JSON.
    std::fs::write(path("not-json.json"), b"this is not json").unwrap();
    let (status, _stdout, _stderr) =
        run_smoke_runner(&["--manifest", path("not-json.json").to_str().unwrap()]);
    assert!(!status.success(), "invalid JSON should be rejected");

    // Case 2: wrong schema_version.
    let m1 = r#"{
      "schema_version": "harness-manifest-v1",
      "harness_id": "test",
      "kind": "context.prepare.v0",
      "entrypoint": { "command": "echo", "cwd": "." },
      "health": { "url": "http://127.0.0.1:17400/health", "expected_status": 200 },
      "endpoint": { "url": "http://127.0.0.1:17400/ctx", "local_only": true },
      "permissions": { "read_paths": [], "network": [] },
      "smoke": { "command": "echo ok" },
      "rollback": { "strategy": "x" }
    }"#;
    std::fs::write(path("bad-schema.json"), m1).unwrap();
    let (status, stdout, _stderr) =
        run_smoke_runner(&["--manifest", path("bad-schema.json").to_str().unwrap()]);
    assert!(!status.success(), "wrong schema_version should be rejected");
    assert_failed_check(&stdout, "manifest schema");

    // Case 3: wrong kind.
    let m2 = m1.replace(
        r#""kind": "context.prepare.v0""#,
        r#""kind": "ingress.route.v0""#,
    );
    std::fs::write(path("bad-kind.json"), m2).unwrap();
    let (status, stdout, _stderr) =
        run_smoke_runner(&["--manifest", path("bad-kind.json").to_str().unwrap()]);
    assert!(!status.success(), "wrong kind should be rejected");
    assert_failed_check(&stdout, "manifest schema");

    // Case 4: missing endpoint.url.
    let m3 = m1.replace(
        r#""endpoint": { "url": "http://127.0.0.1:17400/ctx", "local_only": true },"#,
        "",
    );
    std::fs::write(path("no-endpoint.json"), m3).unwrap();
    let (status, stdout, _stderr) =
        run_smoke_runner(&["--manifest", path("no-endpoint.json").to_str().unwrap()]);
    assert!(!status.success(), "missing endpoint should be rejected");
    assert_failed_check(&stdout, "manifest schema");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn smoke_runner_does_not_modify_kernel_env() {
    // Check the real home harness dir before.
    let home = std::env::var("HOME").expect("HOME must be set");
    let harness_dir = PathBuf::from(home).join(".agent-core").join("harnesses");
    let before_exists = harness_dir.exists();
    let before_entries: Vec<String> = if before_exists {
        std::fs::read_dir(&harness_dir)
            .expect("failed to read harness dir")
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .collect()
    } else {
        vec![]
    };

    // Run smoke runner with a non-existent manifest.
    let tmp = unique_temp_dir("no_modify");
    std::fs::create_dir_all(&tmp).unwrap();
    let fake_manifest = tmp.join("nonexistent.json");

    let (status, _stdout, _stderr) =
        run_smoke_runner(&["--manifest", fake_manifest.to_str().unwrap()]);
    assert!(!status.success(), "should fail with non-existent manifest");

    // Verify no new files were created in the real harness dir.
    if before_exists {
        let after_entries: Vec<String> = std::fs::read_dir(&harness_dir)
            .expect("failed to read harness dir")
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .collect();
        assert_eq!(
            before_entries, after_entries,
            "smoke runner should not create or remove files in harness dir"
        );
    } else {
        assert!(
            !harness_dir.exists(),
            "smoke runner should not create harness dir"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}
