use crate::hook::*;

#[test]
fn context_hook_kind_is_stable() {
    assert_eq!(
        serde_json::to_string(&HookKind::ContextPrepareV0).unwrap(),
        r#""context.prepare.v0""#
    );
}

#[test]
fn disabled_binding_is_the_default() {
    let config = HookConfig::default();
    assert!(!config.enabled);
    assert_eq!(config.failure_mode, HookFailureMode::Disabled);
    assert!(config.provider_id.is_empty());
}

#[test]
fn enabled_binding_requires_endpoint_identity_and_credential() {
    let mut config = HookConfig {
        enabled: true,
        kind: HookKind::ContextPrepareV0,
        endpoint: HookEndpoint {
            url: "http://127.0.0.1:9000/context.prepare.v0".into(),
        },
        failure_mode: HookFailureMode::FailClosed,
        ..Default::default()
    };
    assert!(config.validate().is_err());
    config.provider_id = "provider-a".into();
    assert!(config.validate().is_err());
    config.shared_secret = "secret".into();
    assert!(config.validate().is_ok());
}

#[test]
fn transport_limits_are_finite() {
    let limits = HookLimits::default();
    assert!(limits.timeout_ms > 0);
    assert!(limits.max_request_bytes > 0);
    assert!(limits.max_response_bytes > 0);
    assert!(limits.validate().is_ok());
}

#[test]
fn hook_config_debug_redacts_credential() {
    let config = HookConfig {
        shared_secret: "PRIVATE_SHARED_SECRET".into(),
        ..Default::default()
    };
    let debug = format!("{config:?}");
    assert!(!debug.contains("PRIVATE_SHARED_SECRET"));
    assert!(debug.contains("[REDACTED]"));
}
