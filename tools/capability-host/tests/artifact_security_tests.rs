mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;

fn root(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "capability_host_artifact_{label}_{}_{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn predictable_legacy_temp_prebuild_cannot_replace_verified_artifact() {
    let root = root("prebuild");
    let calculator = fixture_path!("calculator");
    let malicious = fixture_path!("nonzero-exit");
    let digest = store_artifact(&root, &calculator);
    let legacy = std::env::temp_dir().join(format!("capability_artifact_{digest}"));
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::copy(malicious, legacy.join("artifact")).unwrap();
    std::fs::set_permissions(
        legacy.join("artifact"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let (port, _) = start_capability_host(&root);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let (code, deployed) = deploy_calculator(
        &root,
        port,
        &digest,
        "proposal-prebuild",
        "decision-prebuild",
        "snapshot-prebuild",
    );
    assert_eq!(code, 200, "{deployed}");
    let request = serde_json::json!({
        "protocol_version":"external-harness-v1",
        "invocation_id":"prebuild-execute",
        "operation":"external.calculator",
        "arguments":{"operation":"multiply","a":6,"b":7},
        "manifest_id":deployed["manifest_id"],
        "artifact_digest":digest,
        "registry_snapshot_id":"snapshot-prebuild",
    });
    let (_, response) = send_http("127.0.0.1", port, &request.to_string());
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["result"], 42);
    let _ = std::fs::remove_dir_all(legacy);
}

#[test]
fn path_replacement_and_parent_secret_environment_do_not_change_held_inode() {
    let root = root("replacement");
    let calculator = fixture_path!("calculator");
    let digest = store_artifact(&root, &calculator);
    let artifact = capability_host::artifact::resolve_artifact(&root, &digest).unwrap();
    let replacement_path = if let Some(path) = artifact.materialized_path() {
        let path = path.to_path_buf();
        assert!(path.exists(), "non-Linux fallback must exist until spawn");
        Some(path)
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    {
        let fd_path = artifact.verified_execution_path().unwrap();
        let _ = std::fs::set_permissions(&fd_path, std::fs::Permissions::from_mode(0o700));
        if let Ok(mut attacker) = std::fs::OpenOptions::new().write(true).open(&fd_path) {
            assert!(
                std::io::Write::write_all(&mut attacker, b"malicious replacement").is_err(),
                "sealed memfd accepted an attacker write"
            );
        }
    }

    let old_control = std::env::var_os("CAPABILITY_HOST_CONTROL_TOKEN");
    let old_execution = std::env::var_os("CAPABILITY_HOST_EXECUTION_TOKEN");
    std::env::set_var("CAPABILITY_HOST_CONTROL_TOKEN", "parent-secret-control");
    std::env::set_var("CAPABILITY_HOST_EXECUTION_TOKEN", "parent-secret-execution");
    let input = serde_json::json!({
        "protocol_version":"process-harness-v1",
        "operation_name":"external.calculator",
        "invocation_id":"replacement-test",
        "arguments":{"operation":"multiply","a":6,"b":7},
    });
    let output = capability_host::process::run_artifact(
        &artifact,
        &input.to_string(),
        std::time::Duration::from_secs(3),
        65536,
        65536,
    )
    .unwrap();
    restore_env("CAPABILITY_HOST_CONTROL_TOKEN", old_control);
    restore_env("CAPABILITY_HOST_EXECUTION_TOKEN", old_execution);

    assert_eq!(
        output.exit_code,
        Some(0),
        "stdout={} stderr={}",
        output.stdout,
        output.stderr
    );
    let response: serde_json::Value = serde_json::from_str(output.stdout.trim()).unwrap();
    assert_eq!(response["result"], 42);
    if let Some(path) = replacement_path {
        let _ = std::fs::remove_file(path);
    }
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(value) = value {
        std::env::set_var(name, value);
    } else {
        std::env::remove_var(name);
    }
}
