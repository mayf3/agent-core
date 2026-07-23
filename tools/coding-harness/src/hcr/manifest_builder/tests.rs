// ── Tests for version_query, version_allocation, delivery_manifest ──

#[cfg(test)]
mod tests {
    use super::super::service_manifest::build_service_manifest;
    use super::super::version_allocation::{allocate_next_version, increment_patch};
    use super::super::version_query::{parse_status_code, query_deployed_version};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;

    struct EnvironmentReset;

    impl Drop for EnvironmentReset {
        fn drop(&mut self) {
            std::env::remove_var("AGENT_CORE_DEPLOYMENT_HARNESS_READ_URL");
            std::env::remove_var("AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN");
        }
    }

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

    // ── increment_patch tests ──
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

    // ── build_service_manifest tests ──
    #[test]
    fn build_service_manifest_sets_correct_version() {
        let component = serde_json::json!({
            "schema_version": "component-artifact-v1",
            "component_id": "test-component",
            "kind": "hook_consumer_service",
            "target_kind": "HookConsumerService",
            "profile_id": "hook-consumer-service-v0",
            "contract_catalog_version": "1",
            "required_contracts": ["event.observe.v0"],
            "requested_permissions": ["journal.observe"],
            "service": { "version": "0.1.5", "healthcheck_path": "/health" }
        });
        let manifest = build_service_manifest(
            &component,
            "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234",
        )
        .unwrap();
        assert_eq!(manifest.version, "0.1.5");
        assert_eq!(manifest.component_id, "test-component");
        assert_eq!(manifest.entrypoint, "artifact");
    }

    #[test]
    fn build_service_manifest_different_versions_different_ids() {
        let base = serde_json::json!({
            "schema_version": "component-artifact-v1",
            "component_id": "token-dashboard",
            "kind": "hook_consumer_service",
            "target_kind": "HookConsumerService",
            "profile_id": "hook-consumer-service-v0",
            "contract_catalog_version": "1",
            "required_contracts": ["event.observe.v0"],
            "requested_permissions": ["journal.observe"],
            "service": { "version": "0.1.0", "healthcheck_path": "/health" }
        });
        let m1 = build_service_manifest(
            &base,
            "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234",
        )
        .unwrap();
        let mut v2 = base.clone();
        v2["service"]["version"] = serde_json::json!("0.1.1");
        let m2 = build_service_manifest(
            &v2,
            "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234",
        )
        .unwrap();
        assert_ne!(m1.manifest_id, m2.manifest_id);
    }

    // ── Mock server helpers ──
    fn start_mock(response: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
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
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = EnvironmentReset;
        let port = start_mock(response);
        std::env::set_var(
            "AGENT_CORE_DEPLOYMENT_HARNESS_READ_URL",
            format!("http://127.0.0.1:{port}"),
        );
        std::env::set_var("AGENT_CORE_DEPLOYMENT_HARNESS_READ_TOKEN", token);
        thread::sleep(Duration::from_millis(50));
        f();
    }

    // ── query_deployed_version fail‑closed tests ──
    #[test]
    fn version_query_404_returns_none() {
        let resp = b"HTTP/1.1 404 Not Found\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert_eq!(query_deployed_version("test-component").unwrap(), None);
        });
    }
    #[test]
    fn version_query_200_returns_version() {
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true,\"version\":\"0.1.0\"}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert_eq!(
                query_deployed_version("test-component").unwrap(),
                Some("0.1.0".into())
            );
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
            assert!(format!("{err:?}").contains("401"), "expected 401");
        });
    }
    #[test]
    fn version_query_403_fails_closed() {
        let resp = b"HTTP/1.1 403 Forbidden\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let err = query_deployed_version("test-component").unwrap_err();
            assert!(format!("{err:?}").contains("403"), "expected 403");
        });
    }
    #[test]
    fn version_query_500_fails_closed() {
        let resp = b"HTTP/1.1 500 Internal Server Error\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            let err = query_deployed_version("test-component").unwrap_err();
            assert!(format!("{err:?}").contains("500"), "expected 500");
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

    // ── allocate_next_version propagation tests ──
    #[test]
    fn version_allocation_returns_next_patch_when_component_exists() {
        let resp = b"HTTP/1.1 200 OK\r\n\r\n{\"ok\":true,\"version\":\"0.1.0\"}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert_eq!(
                allocate_next_version("test-component").unwrap(),
                Some("0.1.1".into())
            );
        });
    }
    #[test]
    fn version_allocation_returns_none_when_not_deployed() {
        let resp = b"HTTP/1.1 404 Not Found\r\n\r\n{}";
        with_server(resp, "test-token-32-chars-minimum-length!!", || {
            assert_eq!(allocate_next_version("test-component").unwrap(), None);
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
