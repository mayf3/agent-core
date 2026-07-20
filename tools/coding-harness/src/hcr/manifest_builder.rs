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

/// Environment variable for the DH control URL (read‑only usage).
const ENV_DH_CONTROL_URL: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL";
const DEFAULT_DH_URL: &str = "http://127.0.0.1:7400";

/// Environment variable for the DH control token.
const ENV_DH_CONTROL_TOKEN: &str = "AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_TOKEN";

/// Query the Deployment Harness for the current installed version of
/// a component.  Returns `None` when the component does not exist
/// (HTTP 404).  Returns an error on transport/parse failures.
///
/// # Read‑only guarantee
///
/// This function only issues `GET /v1/components/{component_id}`.
/// It never performs deploy, disable, rollback, or any write
/// operation.
pub fn query_deployed_version(component_id: &str) -> Result<Option<String>> {
    let base_url = std::env::var(ENV_DH_CONTROL_URL)
        .unwrap_or_else(|_| DEFAULT_DH_URL.to_string());
    let token = std::env::var(ENV_DH_CONTROL_TOKEN)
        .map_err(|_| anyhow!("MISSING_DH_CONTROL_TOKEN"))?;

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

    let body = extract_http_body(&raw);
    let resp: Value =
        serde_json::from_slice(body).map_err(|e| anyhow!("DH_JSON: {e}"))?;

    // 404 → component not yet deployed
    if raw
        .windows(12)
        .any(|w| w == b"HTTP/1.1 404")
    {
        return Ok(None);
    }
    // 200 → extract version
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        if let Some(ver) = resp.get("version").and_then(|v| v.as_str()) {
            return Ok(Some(ver.to_string()));
        }
    }
    // Any other status → component unknown or error
    Ok(None)
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
}
