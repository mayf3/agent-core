//! Real end-to-end self-deployment canary for `EXTERNAL_COMPONENT_SELF_DEPLOYMENT_V0`.
//!
//! Chain proven here, with **zero Kernel/Runtime code touched**:
//!
//!   real git commit (HEAD)
//!     -> `external.component_build` (this crate's `component_build`)
//!        produces an immutable artifact (release binary) + immutable
//!        `ServiceManifest`, both content-addressed in the shared CAS
//!     -> `external.component_deploy` (the independent `component-deploy-adapter`,
//!        which POSTs to the Deployment Harness's UNCHANGED `POST /v1/deployments`)
//!     -> the real `generated-hook-consumer` process is installed, started under
//!        `env_clear` with the managed-service-v0 env, and probed
//!     -> `GET /health` returns 200 with the `X-Agent-Core-*` identity headers
//!        (the component only goes ready after it successfully polls a real
//!        `event.observe.v0` stream, served here by a stub observer)
//!     -> Receipt carries `status=healthy`, `health_status=ready`, and a
//!        `rollback_ref` on the second (upgraded) deploy.
//!
//! Also asserts: replay idempotency (identical re-deploy returns `replayed=true`,
//! same `deployment_id`), and rollback reference on version upgrade.
//!
//! Marked `#[ignore]` because it shells out to `git` + `cargo build --release`
//! (slow). Run explicitly: `cargo test -p coding-harness --test
//! component_self_deploy_canary -- --ignored`.

use coding_harness::component_build::{build_component, BuildArgs, BuildConfig, ComponentBuildEntry};
use deployment_harness::component_deploy_adapter::{serve_listener as serve_adapter, AdapterConfig};
use deployment_harness::config::DeploymentHarnessConfig;
use deployment_harness::server::serve_listener as serve_deployment_harness;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

/// Repo root = four levels up from this crate's CARGO_MANIFEST_DIR.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .expect("repo root")
}

fn git_head() -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root())
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse HEAD");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn entry(version: &str) -> ComponentBuildEntry {
    ComponentBuildEntry {
        source_root: repo_root(),
        allowed_ref: "HEAD".into(),
        template_relpath: PathBuf::from("tools/coding-harness/templates/hook-consumer-service"),
        // The generator's known-good model output for a hook-consumer projection:
        // it compiles under the canonical prelude (no elided-lifetime returns, no
        // shadowing of `support::html_escape`).
        projection_relpath: PathBuf::from(
            "tools/coding-harness/src/self_evolution/generator/hook_consumer/tests.rs",
        ),
        projection_const: Some("KNOWN_GOOD_TOKEN_SOURCE".into()),
        version: version.into(),
    }
}

/// A stub `event.observe.v0` server: serves an empty event page so the
/// generated-hook-consumer observer goes ready and `/health` returns 200.
struct ObserverStub {
    addr: String,
    token: String,
}

fn start_observer_stub() -> ObserverStub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let token = "observer-token-0123456789abcdef0123456789abcdef".to_string();
    let token_clone = token.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let token = token_clone.clone();
            thread::spawn(move || {
                let _ = handle_observer(&mut stream, &token);
            });
        }
    });
    ObserverStub {
        addr: format!("127.0.0.1:{}", addr.port()),
        token,
    }
}

fn handle_observer(stream: &mut TcpStream, expected_token: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let authorized = request
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("authorization: bearer "))
        .map(|l| l.trim().to_string())
        .map(|l| l["authorization: bearer ".len()..].trim().to_string())
        == Some(expected_token.to_string());
    let body = br#"{"schema_version":"event.observe.v0","events":[],"next_cursor":0,"has_more":false}"#;
    let (status_line, body_out): (&str, &[u8]) = if authorized && request.contains("GET /v1/events") {
        ("200 OK", body)
    } else {
        ("404 Not Found", b"{}")
    };
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_out.len(),
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(body_out)?;
    Ok(())
}

/// HTTP POST a JSON body, return (status_code, body_string).
fn http_post(url: &str, bearer: Option<&str>, body: &[u8]) -> (u16, String) {
    let authority = url.strip_prefix("http://").unwrap_or(url);
    let authority = authority.split('/').next().unwrap_or(authority);
    let mut stream = TcpStream::connect(authority).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    let auth = bearer
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /v1/invocations HTTP/1.1\r\nHost: {authority}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body_str = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("").to_string();
    (status, body_str)
}

/// Probe a deployed component's `/health` directly; return (status, headers).
fn probe_health(endpoint: &str) -> (u16, String) {
    let authority = endpoint.strip_prefix("http://").unwrap_or(endpoint);
    let mut stream = TcpStream::connect(authority).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    stream
        .write_all(format!("GET /health HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, text)
}

/// Build + store the artifact/manifest for `generated-hook-consumer` at a given
/// version, from a real commit.
fn build_at(cas: &Path, version: &str, commit: &str) -> (String, String) {
    let mut entries = HashMap::new();
    entries.insert("generated-hook-consumer".into(), entry(version));
    let cfg = BuildConfig { entries };
    let args = BuildArgs {
        component_id: "generated-hook-consumer".into(),
        source_commit: commit.into(),
    };
    let out = build_component(&args, &cfg, cas).expect("component_build must succeed");
    assert_eq!(out.version, version);
    assert_eq!(out.source_commit, commit);
    (out.artifact_digest, out.manifest_digest)
}

fn deploy_via_adapter(
    adapter_url: &str,
    component_id: &str,
    artifact_digest: &str,
    manifest_digest: &str,
) -> Value {
    let body = serde_json::json!({
        "protocol_version":"external-harness-v1",
        "invocation_id":"canary-invocation",
        "operation":"external.component_deploy",
        "arguments":{
            "component_id": component_id,
            "artifact_digest": artifact_digest,
            "manifest_digest": manifest_digest,
        },
        "manifest_id":"adapter",
        "artifact_digest": artifact_digest,
        "registry_snapshot_id":"canary",
    });
    let (status, body_str) = http_post(adapter_url, None, &serde_json::to_vec(&body).unwrap());
    assert_eq!(status, 200, "adapter HTTP status");
    let resp: Value = serde_json::from_str(&body_str).expect("adapter response JSON");
    resp
}

// Ensure the slow build target dir is compiled once and shared across tests in
// the same binary (best-effort).
static BUILD_GUARD: OnceLock<()> = OnceLock::new();

#[test]
#[ignore = "shells out to git + cargo build --release; run with --ignored"]
fn generated_hook_consumer_self_deploy_canary() {
    BUILD_GUARD.get_or_init(|| ());
    let head = git_head();
    let repo = repo_root();
    let cas = tempfile::tempdir().unwrap();
    let cas_path = canonical(cas.path());

    // ── 1. BUILD: real commit -> immutable artifact + manifest in CAS ───────
    let (artifact_v1, manifest_v1) = build_at(&cas_path, "0.1.0", &head);

    // ── 2. Start stub event-observe server (component readiness dependency) ─
    let observer = start_observer_stub();

    // ── 3. Start the UNCHANGED Deployment Harness (pure CAS consumer) ───────
    let state = tempfile::tempdir().unwrap();
    let state_path = canonical(state.path());
    let dh_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dh_addr = dh_listener.local_addr().unwrap();
    let control_token = "control-token-0123456789abcdef0123456789abcdef".to_string();
    let dh_config = DeploymentHarnessConfig {
        listen_addr: dh_addr,
        artifact_root: cas_path.clone(),
        state_root: state_path,
        control_token: control_token.clone(),
        read_token: None,
        event_observe_url: format!("http://{}/v1/events", observer.addr),
        event_observe_token: observer.token.clone(),
    };
    dh_config.validate().expect("deployment-harness config must validate");
    thread::spawn(move || {
        let _ = serve_deployment_harness(dh_listener, dh_config);
    });

    // ── 4. Start the independent component-deploy-adapter ───────────────────
    let adapter_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let adapter_addr = adapter_listener.local_addr().unwrap();
    let adapter_cfg = AdapterConfig {
        listen_addr: format!("127.0.0.1:{}", adapter_addr.port()),
        artifact_root: cas_path.clone(),
        deployment_harness_url: format!("http://{}", dh_addr),
        deployment_harness_token: control_token,
        incoming_bearer: None,
    };
    thread::spawn(move || {
        let _ = serve_adapter(adapter_listener, adapter_cfg);
    });
    let adapter_url = format!("http://{}", adapter_addr);
    thread::sleep(Duration::from_millis(200));

    // ── 5. DEPLOY v0.1.0 via the adapter -> health PASS ─────────────────────
    let resp1 = deploy_via_adapter(&adapter_url, "generated-hook-consumer", &artifact_v1, &manifest_v1);
    assert_eq!(resp1.get("ok").and_then(Value::as_bool), Some(true), "deploy v1 ok: {resp1}");
    let result1 = resp1.get("result").expect("result1");
    assert_eq!(result1.get("status").and_then(Value::as_str), Some("healthy"));
    assert_eq!(result1.get("health_status").and_then(Value::as_str), Some("ready"));
    assert!(result1.get("rollback_ref").unwrap().is_null(), "first deploy has no rollback ref");
    let deployment_id = result1
        .get("deployment_id")
        .and_then(Value::as_str)
        .expect("deployment_id")
        .to_string();
    let endpoint = result1
        .get("endpoint")
        .and_then(Value::as_str)
        .expect("endpoint")
        .to_string();

    // ── 6. Probe the deployed component's /health directly ──────────────────
    let (hstatus, htext) = probe_health(&endpoint);
    assert_eq!(hstatus, 200, "direct /health must be 200");
    assert!(htext.contains("X-Agent-Core-Component: generated-hook-consumer"), "identity header");
    assert!(htext.contains("X-Agent-Core-Version: 0.1.0"), "version identity header");
    assert!(htext.contains("X-Agent-Core-Instance:"), "instance identity header");

    // ── 7. REPLAY: identical re-deploy must be idempotent ───────────────────
    let resp2 = deploy_via_adapter(&adapter_url, "generated-hook-consumer", &artifact_v1, &manifest_v1);
    let result2 = resp2.get("result").expect("result2");
    assert_eq!(
        result2.get("deployment_id").and_then(Value::as_str),
        Some(deployment_id.as_str()),
        "replay keeps the same deployment_id"
    );
    assert_eq!(result2.get("replayed").and_then(Value::as_bool), Some(true), "replay flag");

    // ── 8. UPGRADE: v0.2.0 -> rollback_ref points at the previous artifact ──
    let (artifact_v2, manifest_v2) = build_at(&cas_path, "0.2.0", &head);
    let resp3 = deploy_via_adapter(&adapter_url, "generated-hook-consumer", &artifact_v2, &manifest_v2);
    let result3 = resp3.get("result").expect("result3");
    assert_eq!(result3.get("status").and_then(Value::as_str), Some("healthy"));
    assert_eq!(
        result3.get("version").and_then(Value::as_str),
        Some("0.2.0"),
        "upgraded version is active"
    );
    let rollback = result3.get("rollback_ref").and_then(Value::as_str);
    assert_eq!(rollback, Some(artifact_v1.as_str()), "rollback ref must reference previous artifact");

    // ── 9. Kernel/Runtime diff invariant: this test depends on agent-core-kernel
    //       and deployment-harness only as libraries; it does not modify any
    //       Kernel or Runtime source. The RepositorySnapshot is a Kernel concept
    //       and is never contacted by either operation (build is in-process here;
    //       the adapter/deployment-harness path has no Kernel client). Verified
    //       separately via `git diff src/ agent-runtime/`.
    let _ = repo; // silence unused warning if any
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::create_dir_all(p).unwrap();
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // ensure_safe_root chmods 0700; replicate so the build's CAS writes remain
    // owner-accessible and the harness validates cleanly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&canon, std::fs::Permissions::from_mode(0o700));
    }
    canon
}
