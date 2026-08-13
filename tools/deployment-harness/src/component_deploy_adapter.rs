//! Component-deploy adapter — an **independent userspace process**, deliberately
//! NOT part of the Deployment Harness frozen core.
//!
//! Boundary: the Deployment Harness binary (`src/main.rs` together with
//! `server.rs`, `manager.rs`, `process.rs`, `config.rs`) is not modified by this
//! module and keeps its pure immutable-artifact CAS-consumer semantics
//! (`deploy → health → rollback → Receipt`). This adapter is a separate loopback
//! service, named `component-deploy-adapter`, with its own env namespace and its
//! own bearer token. It has no access to the Kernel DB, journal, or IPC tokens.
//!
//! What it does: it exposes the `external.component_deploy` operation on the
//! generic `external-harness-v1` Invocation protocol that the Kernel's external
//! harness transport speaks, and translates it into the Deployment Harness's
//! frozen `deployment.effect.v0` deploy protocol. Concretely it:
//!
//! 1. receives `{component_id, artifact_digest, manifest_digest}` as the
//!    operation arguments,
//! 2. loads the immutable `ServiceManifest` from the shared CAS by
//!    `manifest_digest` to obtain `version` and `component_id`, cross-checking
//!    `artifact_digest`/`component_id` against the request,
//! 3. synthesizes a self-validating `DeploymentIntent` with deterministic
//!    governance ids (so the Deployment Harness replay path fires on retries),
//! 4. POSTs it to the Deployment Harness's UNCHANGED `POST /v1/deployments`,
//! 5. re-wraps the bare `DeploymentReceipt` into the
//!    `{protocol_version:"external-harness-v1", ok:true, result:{...}}` envelope.
//!
//! Definitive failures are returned as `{ok:false, error_code}` on HTTP 200 so
//! the generic transport classifies them as harness failures rather than
//! transport `http_error`s.

use agent_core_kernel::capabilities::store::{ContentStore, Sha256Digest};
use agent_core_kernel::domain::{
    DeploymentIntent, DeploymentReceipt, ServiceManifest, DEPLOYMENT_PROTOCOL,
};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const PROTOCOL_VERSION: &str = "external-harness-v1";
const OPERATION: &str = "external.component_deploy";
const MAX_BODY: usize = 128 * 1024;

/// Independent adapter configuration, read from its own env namespace. It shares
/// only the CAS root and the Deployment Harness control credentials with the
/// surrounding infra.
pub struct AdapterConfig {
    pub listen_addr: String,
    pub artifact_root: PathBuf,
    pub deployment_harness_url: String,
    pub deployment_harness_token: String,
    /// Optional bearer required on incoming invocations. When unset, the
    /// adapter relies on loopback isolation (matching the Deployment Harness
    /// fixture-service precedent).
    pub incoming_bearer: Option<String>,
}

impl AdapterConfig {
    pub fn from_env() -> Result<Self> {
        let listen_addr = std::env::var("COMPONENT_DEPLOY_ADAPTER_LISTEN_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:7410".to_string());
        let artifact_root = PathBuf::from(
            std::env::var("DEPLOYMENT_HARNESS_ARTIFACT_ROOT")
                .map_err(|_| anyhow!("DEPLOYMENT_HARNESS_ARTIFACT_ROOT not set"))?,
        );
        let deployment_harness_url = std::env::var("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7400".to_string());
        let deployment_harness_token = std::env::var("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_TOKEN")
            .map_err(|_| anyhow!("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_TOKEN not set"))?;
        let incoming_bearer = std::env::var("COMPONENT_DEPLOY_ADAPTER_BEARER")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        // Enforce loopback listen — this adapter never binds off-loopback.
        resolve_loopback_addr(&listen_addr)
            .map_err(|_| anyhow!("COMPONENT_DEPLOY_ADAPTER_LISTEN_ADDR must be loopback"))?;
        // The Deployment Harness endpoint must also be loopback.
        resolve_loopback_authority(&deployment_harness_url)
            .map_err(|_| anyhow!("AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL must be loopback"))?;
        Ok(Self {
            listen_addr,
            artifact_root,
            deployment_harness_url,
            deployment_harness_token,
            incoming_bearer,
        })
    }
}

/// Immutable deployment inputs supplied by the Agent (carried from the build
/// Receipt). Unknown fields are rejected.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployArgs {
    pub component_id: String,
    pub artifact_digest: String,
    pub manifest_digest: String,
}

/// Core translation: immutable digests → Deployment Harness receipt, wrapped as
/// the external-harness-v1 `result` object. Returns a stable `error_code` on
/// failure so the HTTP layer can emit `{ok:false, error_code}` on HTTP 200.
pub fn translate(args: &DeployArgs, cfg: &AdapterConfig) -> Result<Value, String> {
    // 1. Load the immutable ServiceManifest from the shared CAS.
    let store = ContentStore::new(cfg.artifact_root.clone());
    let manifest_digest = Sha256Digest::parse(&args.manifest_digest)
        .map_err(|_| "manifest_digest_invalid".to_string())?;
    let manifest_bytes = store
        .load(&manifest_digest)
        .map_err(|_| "manifest_not_in_cas".to_string())?;
    let manifest: ServiceManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| "service_manifest_invalid".to_string())?;

    // 2. Cross-check immutable inputs against the manifest. These digests are
    //    the only thing the Agent controls; they must agree with the manifest
    //    the Deployment Harness will independently load.
    if manifest.artifact_digest != args.artifact_digest {
        return Err("artifact_digest_mismatch".to_string());
    }
    if manifest.component_id != args.component_id {
        return Err("component_id_mismatch".to_string());
    }

    // 3. Synthesize a deterministic, self-validating DeploymentIntent.
    let intent = build_intent(
        &args.component_id,
        &args.artifact_digest,
        &args.manifest_digest,
        &manifest.version,
    );

    // 4. POST to the Deployment Harness's unchanged deploy route.
    let receipt = post_deploy(cfg, &intent)?;

    // 5. Wrap into the external-harness-v1 result object.
    Ok(wrap_result(&receipt))
}

/// Construct a `DeploymentIntent` whose governance ids are a deterministic
/// function of the immutable inputs only. This makes a repeated deploy of the
/// same `(component_id, artifact_digest, manifest_digest)` a replay at the
/// Deployment Harness (identical intent → `replayed = true`), independent of the
/// Kernel invocation id.
fn build_intent(
    component_id: &str,
    artifact_digest: &str,
    manifest_digest: &str,
    version: &str,
) -> DeploymentIntent {
    let fingerprint = serde_json::to_vec(&json!({
        "component_id": component_id,
        "artifact_digest": artifact_digest,
        "manifest_digest": manifest_digest,
    }))
    .unwrap_or_default();
    let short = &hex::encode(Sha256::digest(fingerprint))[..16];
    let mut intent = DeploymentIntent {
        protocol_version: DEPLOYMENT_PROTOCOL.into(),
        invocation_id: format!("component_deploy_invocation_{short}"),
        intent_id: String::new(),
        proposal_id: format!("component_deploy_proposal_{short}"),
        decision_id: format!("component_deploy_decision_{short}"),
        service_manifest_digest: manifest_digest.into(),
        artifact_digest: artifact_digest.into(),
        expected_version: version.into(),
        action: "install_start".into(),
    };
    // intent_id is a stable hash over the other intent fields.
    intent.intent_id = intent.expected_intent_id();
    intent
}

/// Wrap a validated `DeploymentReceipt` as the harness `result` object.
fn wrap_result(receipt: &DeploymentReceipt) -> Value {
    json!({
        "component_id": receipt.component_id,
        "deployment_id": receipt.deployment_id,
        "artifact_digest": receipt.artifact_digest,
        "service_manifest_digest": receipt.service_manifest_digest,
        "version": receipt.version,
        "status": receipt.status,
        "health_status": receipt.health_status,
        "endpoint": receipt.endpoint,
        "rollback_ref": receipt.previous_artifact_digest,
        "replayed": receipt.replayed,
    })
}

/// POST a `DeploymentIntent` to the Deployment Harness `/v1/deployments` route
/// and return the bare `DeploymentReceipt` on success, or a stable `error_code`
/// on definitive rejection. Non-2xx that is not a clean definitive envelope is
/// reported as `deployment_harness_effect_uncertain` so the caller never treats
/// an unknown outcome as success.
fn post_deploy(cfg: &AdapterConfig, intent: &DeploymentIntent) -> Result<DeploymentReceipt, String> {
    let addr = resolve_loopback_authority(&cfg.deployment_harness_url)
        .map_err(|_| "deployment_harness_endpoint_invalid".to_string())?;
    let authority = http_authority(&cfg.deployment_harness_url).unwrap_or_default();
    let body = serde_json::to_vec(intent).map_err(|_| "intent_serialize_failed".to_string())?;
    let request = format!(
        "POST /v1/deployments HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        token = cfg.deployment_harness_token,
        len = body.len(),
    );
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .map_err(|_| "deployment_harness_unreachable".to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| "deployment_harness_unreachable".to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| "deployment_harness_unreachable".to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "deployment_harness_write_failed".to_string())?;
    stream
        .write_all(&body)
        .map_err(|_| "deployment_harness_write_failed".to_string())?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() > MAX_BODY {
                    return Err("deployment_harness_response_oversized".to_string());
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return Err("deployment_harness_effect_uncertain".to_string());
            }
            Err(_) => return Err("deployment_harness_read_failed".to_string()),
        }
    }
    let text = String::from_utf8(raw)
        .map_err(|_| "deployment_harness_malformed_response".to_string())?;
    let status_code = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body_str = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("");
    if status_code == 200 {
        let receipt: DeploymentReceipt = serde_json::from_str(body_str)
            .map_err(|_| "deployment_receipt_malformed".to_string())?;
        Ok(receipt)
    } else if (400..500).contains(&status_code) {
        // Definitive rejection — pass the harness's own error_code through.
        let code = serde_json::from_str::<Value>(body_str)
            .ok()
            .and_then(|v| {
                v.get("error_code")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "deployment_failed".to_string());
        Err(code)
    } else {
        Err("deployment_harness_effect_uncertain".to_string())
    }
}

// ── HTTP server (hand-rolled, loopback only) ──────────────────────────────

pub fn serve_listener(listener: TcpListener, config: AdapterConfig) -> Result<()> {
    let config = Arc::new(config);
    for stream in listener.incoming() {
        let mut stream = stream?;
        let config = Arc::clone(&config);
        std::thread::spawn(move || {
            let _ = handle(&mut stream, &config);
        });
    }
    Ok(())
}

fn handle(stream: &mut TcpStream, config: &AdapterConfig) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let request = read_request(stream)?;

    if let Some(expected) = &config.incoming_bearer {
        if request.bearer.as_deref() != Some(expected.as_str()) {
            return write_envelope(
                stream,
                &json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error_code":"unauthorized"}),
            );
        }
    }
    if request.method != "POST" {
        return write_envelope(
            stream,
            &json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error_code":"method_not_allowed"}),
        );
    }

    let parsed: Value = match serde_json::from_slice(&request.body) {
        Ok(v) => v,
        Err(_) => {
            return write_envelope(
                stream,
                &json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error_code":"invalid_json"}),
            );
        }
    };
    if parsed
        .get("protocol_version")
        .and_then(Value::as_str)
        .unwrap_or("")
        != PROTOCOL_VERSION
    {
        return write_envelope(
            stream,
            &json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error_code":"protocol_mismatch"}),
        );
    }
    let op = parsed.get("operation").and_then(Value::as_str).unwrap_or("");
    if op != OPERATION {
        return write_envelope(
            stream,
            &json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error_code":"operation_mismatch"}),
        );
    }
    let args_value = parsed.get("arguments").cloned().unwrap_or(Value::Null);
    let args: DeployArgs = match serde_json::from_value(args_value) {
        Ok(a) => a,
        Err(_) => {
            return write_envelope(
                stream,
                &json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error_code":"invalid_arguments"}),
            );
        }
    };

    match translate(&args, config) {
        Ok(result) => write_envelope(
            stream,
            &json!({"protocol_version":PROTOCOL_VERSION,"ok":true,"result":result}),
        ),
        Err(code) => write_envelope(
            stream,
            &json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error_code":code}),
        ),
    }
}

struct RawRequest {
    method: String,
    bearer: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<RawRequest> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("REQUEST_TRUNCATED");
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break index;
        }
        if bytes.len() > 32 * 1024 {
            bail!("REQUEST_HEADERS_TOO_LARGE");
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = headers.lines();
    let first = lines.next().ok_or_else(|| anyhow!("REQUEST_INVALID"))?;
    let mut first = first.split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    if !matches!(method.as_str(), "GET" | "POST") {
        bail!("REQUEST_INVALID");
    }
    let mut content_length = None;
    let mut bearer = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            bail!("REQUEST_HEADER_INVALID");
        };
        if name.eq_ignore_ascii_case("content-length") {
            let parsed: usize = value.trim().parse()?;
            if content_length.replace(parsed).is_some() {
                bail!("CONTENT_LENGTH_DUPLICATE");
            }
        } else if name.eq_ignore_ascii_case("authorization") {
            bearer = value.trim().strip_prefix("Bearer ").map(str::to_string);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("TRANSFER_ENCODING_NOT_SUPPORTED");
        }
    }
    let expected = content_length.unwrap_or(0);
    if expected > MAX_BODY {
        bail!("REQUEST_BODY_TOO_LARGE");
    }
    let body_start = header_end + 4;
    let mut body = bytes[body_start..].to_vec();
    while body.len() < expected {
        let mut chunk = vec![0u8; (expected - body.len()).min(8192)];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            bail!("REQUEST_BODY_TRUNCATED");
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(expected);
    Ok(RawRequest {
        method,
        bearer,
        body,
    })
}

/// Always respond HTTP 200 with the external-harness-v1 envelope. The generic
/// transport reads `protocol_version`/`ok`/`result`/`error_code` only on 2xx.
fn write_envelope(stream: &mut TcpStream, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(&body)?;
    Ok(())
}

// ── URL / loopback helpers ────────────────────────────────────────────────

fn http_authority(url: &str) -> Option<String> {
    let authority = url.strip_prefix("http://")?;
    Some(authority.split('/').next().unwrap_or(authority).to_string())
}

/// Resolve a `host:port` authority to a single socket addr, requiring every
/// resolved address to be loopback (matching the Kernel transport's policy).
fn resolve_loopback_addr(authority: &str) -> Result<std::net::SocketAddr> {
    let addrs: Vec<_> = authority
        .to_socket_addrs()
        .map_err(|e| anyhow!("resolve failed: {e}"))?
        .collect();
    if addrs.is_empty() {
        bail!("no addresses");
    }
    if !addrs.iter().all(|a| a.ip().is_loopback()) {
        bail!("non-loopback address");
    }
    Ok(addrs[0])
}

/// Parse an `http://host:port[/path]` URL and resolve its authority to a
/// loopback socket addr.
fn resolve_loopback_authority(url: &str) -> Result<std::net::SocketAddr> {
    let authority = http_authority(url).ok_or_else(|| anyhow!("endpoint must use http://"))?;
    resolve_loopback_addr(&authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_kernel::domain::{
        ListenPolicy, RollbackPolicy, ServiceHealthcheck, TargetKind, UpgradePolicy,
        SERVICE_MANIFEST_SCHEMA,
    };

    fn sample_manifest(component_id: &str, artifact_digest: &str, version: &str) -> ServiceManifest {
        let mut m = ServiceManifest {
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
                timeout_ms: 1000,
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
        m.manifest_id = m.compute_manifest_id().unwrap();
        m
    }

    #[test]
    fn build_intent_is_self_validating_and_deterministic() {
        let art_a = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let art_c = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let man = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let a = build_intent("generated-hook-consumer", art_a, man, "0.1.0");
        a.validate().expect("intent must self-validate");
        // Same inputs => identical ids (replay stability).
        let b = build_intent("generated-hook-consumer", art_a, man, "0.1.0");
        assert_eq!(a.intent_id, b.intent_id);
        assert_eq!(a.invocation_id, b.invocation_id);
        assert_eq!(
            a.deployment_id("generated-hook-consumer"),
            b.deployment_id("generated-hook-consumer")
        );
        // Different artifact => different ids.
        let c = build_intent("generated-hook-consumer", art_c, man, "0.1.0");
        assert_ne!(a.intent_id, c.intent_id);
    }

    #[test]
    fn translate_rejects_missing_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = AdapterConfig {
            listen_addr: "127.0.0.1:0".into(),
            artifact_root: tmp.path().to_path_buf(),
            deployment_harness_url: "http://127.0.0.1:7400".into(),
            deployment_harness_token: "t".into(),
            incoming_bearer: None,
        };
        let args = DeployArgs {
            component_id: "generated-hook-consumer".into(),
            artifact_digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            manifest_digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111".into(),
        };
        let err = translate(&args, &cfg).unwrap_err();
        assert_eq!(err, "manifest_not_in_cas");
    }

    #[test]
    fn translate_rejects_artifact_digest_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ContentStore::new(tmp.path().to_path_buf());
        let manifest = sample_manifest(
            "generated-hook-consumer",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "0.1.0",
        );
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let digest = store
            .store(&manifest_bytes)
            .unwrap()
            .as_str()
            .to_string();
        let cfg = AdapterConfig {
            listen_addr: "127.0.0.1:0".into(),
            artifact_root: tmp.path().to_path_buf(),
            deployment_harness_url: "http://127.0.0.1:7400".into(),
            deployment_harness_token: "t".into(),
            incoming_bearer: None,
        };
        let args = DeployArgs {
            component_id: "generated-hook-consumer".into(),
            // Deliberately different artifact digest than the manifest declares.
            artifact_digest: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            manifest_digest: digest,
        };
        let err = translate(&args, &cfg).unwrap_err();
        assert_eq!(err, "artifact_digest_mismatch");
    }

    #[test]
    fn loopback_enforcement_rejects_off_loopback_url() {
        assert!(resolve_loopback_authority("http://192.0.2.1:7400").is_err());
        assert!(resolve_loopback_authority("http://127.0.0.1:7400").is_ok());
    }
}
