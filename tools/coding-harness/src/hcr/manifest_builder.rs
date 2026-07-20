//! Version allocation and delivery manifest construction.
//!
//! Responsibilities moved from Kernel (`coding_task_submit.rs`) to
//! the Coding Harness as part of the external development boundary
//! cleanup.  The Harness queries the Deployment Harness (read-only
//! version endpoint), allocates the next patch version, and constructs
//! the final delivery `ServiceManifest`.
//!
//! # Security
//!
//! The version-query credential must be **read-only** — it shall only
//! permit `GET /v1/components/{id}` (version state), never `POST`,
//! `PUT`, `DELETE`, or any control operation.

use agent_core_kernel::domain::service_manifest::{
    ListenPolicy, RollbackPolicy, SERVICE_MANIFEST_SCHEMA, ServiceHealthcheck, ServiceManifest,
    UpgradePolicy,
};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

// ──────────────────────────────────────────────
//  Read-only version query (Deployment Harness)
// ──────────────────────────────────────────────

/// Environment variable for the DH read‑only URL (falls back to
/// `AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL` when not set).
const ENV_DH_READ_URL: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_READ_URL";
const ENV_DH_CONTROL_URL_FALLBACK: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL";
const DEFAULT_DH_URL: &str = "http://127.0.0.1:7400";

/// Environment variable for the DH read‑only token.
///
/// This token MUST be scoped to only allow `GET /v1/components/{id}`.
/// It must NOT permit deploy, disable, rollback, or any other write
/// operation.  The Deployment Harness enforces this server-side.
const ENV_DH_READ_TOKEN: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN";

/// Query the Deployment Harness for the current installed version of
/// a component.
///
/// # Semantics (fail‑closed)
///
/// | HTTP status | JSON body | Result |
/// |-------------|-----------|--------|
/// | 404         | any       | `Ok(None)` — component does not exist |
/// | 200         | `{"ok":true,"version":"X.Y.Z"}` | `Ok(Some("X.Y.Z"))` |
/// | 200         | missing `version` or empty | `Err` |
/// | 200         | `ok` is not `true` | `Err` |
/// | 401, 403, 409, 429, 5xx | any | `Err` |
/// | transport / timeout / JSON parse | — | `Err` |
///
/// Every error is fail‑closed: the caller MUST NOT interpret a non‑404
/// error as "component does not exist" or silently fall back to a
/// default version.
///
/// # Read‑only guarantee
///
/// This function only issues `GET /v1/components/{component_id}`.
/// It never performs deploy, disable, rollback, or any write
/// operation.  The credential used is the dedicated read‑only token.
pub fn query_deployed_version(component_id: &str) -> Result<Option<String>> {
    let base_url = std::env::var(ENV_DH_READ_URL)
        .or_else(|_| std::env::var(ENV_DH_CONTROL_URL_FALLBACK))
        .unwrap_or_else(|_| DEFAULT_DH_URL.to_string());
    let token = std::env::var(ENV_DH_READ_TOKEN)
        .map_err(|_| anyhow!("MISSING_DH_READ_TOKEN"))?;
    if token.len() < 32 {
        return Err(anyhow!("DH_READ_TOKEN_TOO_SHORT"));
    }

    // Parse the URL to extract host:port and path
    let url = url_parse(&base_url)?;
    let path = format!("{}/v1/components/{}", url.path, component_id);

    let addr = format!("{}:{}", url.host, url.port);
    let mut stream = TcpStream::connect_timeout(
        &addr.parse().map_err(|e| anyhow!("BAD_ADDR: {e}"))?,
        Duration::from_secs(5),
    )
    .map_err(|e| anyhow!("DH_CONNECT: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok();

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n",
        path = path,
        host = url.host,
        token = token,
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| anyhow!("DH_WRITE: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| anyhow!("DH_READ: {e}"))?;

    // ── 1. Parse HTTP status line ──────────────────────────────────
    let status_code = parse_status_code(&raw)?;

    // ── 2. Fail‑closed dispatch by status ──────────────────────────
    match status_code {
        404 => {
            // Component not yet deployed — not an error.
            Ok(None)
        }
        200 => {
            // Must have a valid JSON body with ok=true and version
            let body = extract_http_body(&raw);
            let resp: Value =
                serde_json::from_slice(body).map_err(|e| anyhow!("DH_JSON: {e}"))?;

            if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Err(anyhow!("DH_NOT_OK: component exists but ok!=true"));
            }
            let version = resp
                .get("version")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("DH_MISSING_VERSION"))?;
            Ok(Some(version.to_string()))
        }
        other => {
            // 401, 403, 409, 429, 5xx, or any other unexpected status.
            // Fail closed — do NOT treat these as "component not found".
            Err(anyhow!("DH_UNEXPECTED_STATUS:{other}"))
        }
    }
}

/// Parse the HTTP status code from a raw HTTP/1.1 response.
///
/// Returns `Err` if the status line cannot be parsed (malformed
/// response or connection error).
fn parse_status_code(raw: &[u8]) -> Result<u16> {
    // Find end of status line
    let line_end = raw
        .windows(2)
        .position(|w| w == b"\r\n")
        .ok_or_else(|| anyhow!("DH_NO_STATUS_LINE"))?;
    let status_line = std::str::from_utf8(&raw[..line_end])
        .map_err(|_| anyhow!("DH_STATUS_NOT_UTF8"))?;
    // Expected format: "HTTP/1.1 XXX ..."
    let code_str = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("DH_STATUS_MALFORMED:{status_line}"))?;
    code_str
        .parse::<u16>()
        .map_err(|_| anyhow!("DH_STATUS_NOT_NUMERIC:{code_str}"))
}

/// Increment the patch component of a semver string.
/// Returns `None` if `current` is not a valid three-part semver.
pub fn increment_patch(current: &str) -> Option<String> {
    let parts: Vec<&str> = current.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let major: u64 = parts[0].parse().ok()?;
    let minor: u64 = parts[1].parse().ok()?;
    let patch: u64 = parts[2].parse().ok()?;
    Some(format!("{major}.{minor}.{}", patch.wrapping_add(1)))
}

/// Allocate the next patch version for a managed-service component.
///
/// - If the component exists on the Deployment Harness: returns
///   `Some(increment_patch(current_version))`.
/// - If the component does not exist: returns `None` (the generator's
///   default version, e.g. `"0.1.0"`, is used as-is).
pub fn allocate_next_version(component_id: &str) -> Result<Option<String>> {
    match query_deployed_version(component_id)? {
        Some(current) => {
            let next = increment_patch(&current).ok_or_else(|| {
                anyhow!("INVALID_EXISTING_VERSION: {current}")
            })?;
            Ok(Some(next))
        }
        None => Ok(None),
    }
}

// ──────────────────────────────────────────────
//  Delivery manifest construction
// ──────────────────────────────────────────────

// ⚠ CONCURRENT VERSION ALLOCATION RISK
//
// Two concurrent HCR acceptance flows with different idempotency_keys
// may both query the Deployment Harness at the same time, observe the
// same current version (e.g. "0.1.0"), and independently compute the
// next version ("0.1.1").  Only one succeeds at deployment time; the
// other fails the Deployment Harness monotonicity check.
//
// This is a **known medium-priority debt**:
//  - No data corruption — the DH rejects the non‑monotonic deployment.
//  - The next submission after the conflict correctly allocates the
//    version that follows the first successful deployment.
//  - The HCR ExecutionStore (file lock) only serialises by the same
//    idempotency_key, not across different keys.
//
// The proper fix requires atomic version allocation on the DH side
// or a distributed lock in the acceptance pipeline.

/// Construct the final delivery `ServiceManifest` from a component
/// artifact manifest and the verified artifact digest.
///
/// The version field is already resolved by the time this function
/// is called — it either comes from the generator (initial version)
/// or was overridden via `allocate_next_version`.
pub fn build_delivery_manifest(
    component: &Value,
    artifact_digest: &str,
) -> Result<ServiceManifest> {
    // Validate identity fields
    let target_kind = component
        .get("target_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_TARGET_KIND"))?;
    if target_kind != "HookConsumerService" {
        return Err(anyhow!("UNEXPECTED_TARGET_KIND: {target_kind}"));
    }
    let _schema_version = component
        .get("schema_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_SCHEMA_VERSION"))?;
    let component_id = component
        .get("component_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_COMPONENT_ID"))?;

    let service = component
        .get("service")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("MISSING_SERVICE_OBJECT"))?;

    let version = service
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("MISSING_SERVICE_VERSION"))?;

    let healthcheck_path = service
        .get("healthcheck_path")
        .and_then(|v| v.as_str())
        .unwrap_or("/health");

    // Extract contracts and permissions from the component manifest
    let required_contracts: Vec<String> = component
        .get("required_contracts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| anyhow!("MISSING_REQUIRED_CONTRACTS"))?;

    let requested_permissions: Vec<String> = component
        .get("requested_permissions")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| anyhow!("MISSING_REQUESTED_PERMISSIONS"))?;

    let mut manifest = ServiceManifest {
        schema_version: SERVICE_MANIFEST_SCHEMA.into(),
        manifest_id: String::new(),
        component_id: component_id.into(),
        kind: agent_core_kernel::domain::TargetKind::HookConsumerService,
        artifact_digest: artifact_digest.into(),
        entrypoint: "artifact".into(),
        runtime_profile: "managed-service-v0".into(),
        version: version.into(),
        required_contracts,
        requested_permissions,
        listen_policy: ListenPolicy {
            host: "127.0.0.1".into(),
            port: 0,
            exposure: "loopback".into(),
        },
        healthcheck: ServiceHealthcheck {
            method: "GET".into(),
            path: healthcheck_path.into(),
            timeout_ms: 10_000,
        },
        state_path: "state".into(),
        upgrade_policy: UpgradePolicy {
            strategy: "replace_after_ready".into(),
            require_healthy_before_switch: true,
        },
        rollback_policy: RollbackPolicy {
            retain_previous_versions: 2,
            automatic_on_health_failure: true,
        },
    };
    manifest.manifest_id = manifest.compute_manifest_id()?;
    manifest
        .validate()
        .map_err(|e| anyhow!("MANIFEST_VALIDATION: {e}"))?;
    Ok(manifest)
}

// ──────────────────────────────────────────────
//  Internal helpers
// ──────────────────────────────────────────────

struct ParsedUrlOwned {
    host: String,
    port: u16,
    path: String,
}

/// Minimal URL parser for `http://host:port/path` (no scheme
/// validation beyond stripping `http://`).
fn url_parse(raw: &str) -> Result<ParsedUrlOwned> {
    let without_scheme = raw
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("DH_URL_MUST_BE_HTTP"))?;
    let (host_port, path) = match without_scheme.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (without_scheme, String::new()),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|e| anyhow!("BAD_PORT: {e}"))?),
        None => (host_port.to_string(), 7400u16),
    };
    Ok(ParsedUrlOwned { host, port, path })
}

/// Extract the HTTP body from a raw HTTP/1.1 response.
fn extract_http_body(raw: &[u8]) -> &[u8] {
    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
        &raw[pos + 4..]
    } else {
        raw
    }
}

// ──────────────────────────────────────────────
//  Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicU16, Ordering};
    use std::thread;
    use std::time::Duration;

    // ── parse_status_code tests ────────────────────────────

    #[test]
    fn parse_status_200() {
        assert_eq!(
            parse_status_code(b"HTTP/1.1 200 OK\r\nContent-Type: text\r\n\r\n{}").unwrap(),
            200
        );
    }

    #[test]
    fn parse_status_404() {
        assert_eq!(
            parse_status_code(b"HTTP/1.1 404 Not Found\r\n\r\n{}").unwrap(),
            404
        );
    }

    #[test]
    fn parse_status_401() {
        assert_eq!(
            parse_status_code(b"HTTP/1.1 401 Unauthorized\r\n\r\n{}").unwrap(),
            401
        );
    }

    #[test]
    fn parse_status_500() {
        assert_eq!(
            parse_status_code(b"HTTP/1.1 500 Internal Server Error\r\n\r\n").unwrap(),
            500
        );
    }

    #[test]
    fn parse_status_no_crlf_fails() {
        assert!(parse_status_code(b"HTTP/1.1 200 OK").is_err());
    }

    #[test]
    fn parse_status_empty_fails() {
        assert!(parse_status_code(b"").is_err());
    }

    // ── increment_patch tests ──────────────────────────────

    #[test]
    fn increment_patch_basic() {
        assert_eq!(increment_patch("0.1.0"), Some("0.1.1".into()));
        assert_eq!(increment_patch("0.1.9"), Some("0.1.10".into()));
        assert_eq!(increment_patch("1.0.0"), Some("1.0.1".into()));
    }

    #[test]
    fn increment_patch_invalid() {
        assert_eq!(increment_patch("0.1"), None);
        assert_eq!(increment_patch("0.a.0"), None);
        assert_eq!(increment_patch(""), None);
        assert_eq!(increment_patch("0.1.0.0"), None);
    }

    #[test]
    fn increment_patch_not_equal() {
        let orig = "0.1.0";
        let next = increment_patch(orig).unwrap();
        assert_ne!(orig, next);
    }

    // ── build_delivery_manifest tests ──────────────────────

    #[test]
    fn build_delivery_manifest_sets_correct_version() {
        let component = serde_json::json!({
            "schema_version": "component-artifact-v1",
            "component_id": "test-component",
            "target_kind": "HookConsumerService",
            "profile_id": "hook-consumer-service-v0",
            "contract_catalog_version": "1",
            "required_contracts": ["event.observe.v0"],
            "requested_permissions": ["journal.observe"],
            "service": {
                "version": "0.1.5",
                "healthcheck_path": "/health"
            }
        });
        let manifest =
            build_delivery_manifest(&component, "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234")
                .unwrap();
        assert_eq!(manifest.version, "0.1.5");
        assert_eq!(manifest.component_id, "test-component");
        assert_eq!(manifest.entrypoint, "artifact");
    }

    #[test]
    fn build_delivery_manifest_different_versions_different_ids() {
        let base = serde_json::json!({
            "schema_version": "component-artifact-v1",
            "component_id": "token-dashboard",
            "target_kind": "HookConsumerService",
            "profile_id": "hook-consumer-service-v0",
            "contract_catalog_version": "1",
            "required_contracts": ["event.observe.v0"],
            "requested_permissions": ["journal.observe"],
            "service": {
                "version": "0.1.0",
                "healthcheck_path": "/health"
            }
        });
        let m1 = build_delivery_manifest(
            &base,
            "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234",
        )
        .unwrap();
        let mut v2 = base.clone();
        v2["service"]["version"] = serde_json::json!("0.1.1");
        let m2 = build_delivery_manifest(
            &v2,
            "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234",
        )
        .unwrap();
        assert_ne!(m1.manifest_id, m2.manifest_id);
    }

    // ── Mock server helpers ────────────────────────────────

    /// Start a mock TCP server that responds with a fixed HTTP response.
    /// Returns the port number.
    fn start_mock(response: &'static [u8]) -> u16 {
        static NEXT_PORT: AtomicU16 = AtomicU16::new(18000);
        let port = NEXT_PORT.fetch_add(1, Ordering::SeqCst);
        thread::spawn(move || {
            let addr = format!("127.0.0.1:{port}");
            let listener = TcpListener::bind(&addr).unwrap();
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response);
                let _ = stream.flush();
            }
        });
        port
    }

    fn with_server<F>(response: &'static [u8], token: &str, f: F)
    where
        F: FnOnce(),
    {
        let port = start_mock(response);
        std::env::set_var("AGENT_CORE_DEPLOYMENT_HARNESS_READ_URL", format!("http://127.0.0.1:{port}"));
        std::env::set_var("AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN", token);
        // Give the server thread a moment to start
        thread::sleep(Duration::from_millis(50));
        f();
        std::env::remove_var("AGENT_CORE_DEPLOYMENT_HARNESS_READ_URL");
        std::env::remove_var("AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN");
    }

    // ── query_deployed_version fail‑closed tests ───────────

    #[test]
    fn version_query_404_returns_none() {
        let resp = b"HTTP/1.1 404 Not Found\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let result = query_deployed_version("test-component").unwrap();
            assert_eq!(result, None);
        });
    }

    #[test]
    fn version_query_200_returns_version() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true,\"version\":\"0.1.0\"}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let result = query_deployed_version("test-component").unwrap();
            assert_eq!(result, Some("0.1.0".into()));
        });
    }

    #[test]
    fn version_query_200_missing_version_fails() {
        let resp = b"HTTP/1.1 200 OK\r\n\r\n{\"ok\":true}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert!(query_deployed_version("test-component").is_err());
        });
    }

    #[test]
    fn version_query_200_empty_version_fails() {
        let resp = b"HTTP/1.1 200 OK\r\n\r\n{\"ok\":true,\"version\":\"\"}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert!(query_deployed_version("test-component").is_err());
        });
    }

    #[test]
    fn version_query_401_fails_closed() {
        let resp = b"HTTP/1.1 401 Unauthorized\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let err = query_deployed_version("test-component").unwrap_err();
            let msg = format!("{err:?}");
            assert!(msg.contains("401"), "expected 401 error, got: {msg}");
        });
    }

    #[test]
    fn version_query_403_fails_closed() {
        let resp = b"HTTP/1.1 403 Forbidden\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let err = query_deployed_version("test-component").unwrap_err();
            let msg = format!("{err:?}");
            assert!(msg.contains("403"), "expected 403 error, got: {msg}");
        });
    }

    #[test]
    fn version_query_500_fails_closed() {
        let resp = b"HTTP/1.1 500 Internal Server Error\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let err = query_deployed_version("test-component").unwrap_err();
            let msg = format!("{err:?}");
            assert!(msg.contains("500"), "expected 500 error, got: {msg}");
        });
    }

    #[test]
    fn version_query_malformed_body_fails_closed() {
        let resp = b"HTTP/1.1 200 OK\r\n\r\n{invalid json}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert!(query_deployed_version("test-component").is_err());
        });
    }

    #[test]
    fn version_query_200_ok_not_true_fails() {
        let resp = b"HTTP/1.1 200 OK\r\n\r\n{\"ok\":false}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert!(query_deployed_version("test-component").is_err());
        });
    }

    // ── allocate_next_version propagation tests ────────────

    #[test]
    fn version_allocation_returns_next_patch_when_component_exists() {
        let resp = b"HTTP/1.1 200 OK\r\n\r\n{\"ok\":true,\"version\":\"0.1.0\"}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let result = allocate_next_version("test-component").unwrap();
            assert_eq!(result, Some("0.1.1".into()));
        });
    }

    #[test]
    fn version_allocation_returns_none_when_not_deployed() {
        let resp = b"HTTP/1.1 404 Not Found\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let result = allocate_next_version("test-component").unwrap();
            assert_eq!(result, None);
        });
    }

    #[test]
    fn version_allocation_does_not_fall_back_to_initial_on_query_error() {
        let resp = b"HTTP/1.1 401 Unauthorized\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert!(allocate_next_version("test-component").is_err());
        });
    }
}
