//! Strict validation of Coding Harness acceptance responses.
//!
//! Before writing a Journal Receipt, the Kernel MUST validate every
//! identity field, digest format, gate structure, and outcome
//! consistency (H2). Any mismatch → no Receipt, no settlement.

use serde_json::Value;
use std::collections::HashSet;

/// Context from the outgoing request, used for identity comparison.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub hcr_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub principal_id: String,
    pub gateway_session_id: String,
    pub registry_snapshot_id: String,
    pub operation: String,
    pub idempotency_key: String,
}

/// Result of a successful response validation.
#[derive(Debug)]
pub struct ValidatedResponse {
    pub harness_execution_id: String,
    pub candidate_id: String,
    pub overall_outcome: String,
    pub candidate_digest: String,
    pub artifact_ref: Option<String>,
    pub artifact_digest: Option<String>,
    pub component_manifest_digest: Option<String>,
    pub delivery_manifest_ref: Option<String>,
    pub delivery_manifest_digest: Option<String>,
    pub evidence_digest: String,
    pub gate_count: usize,
}

/// Validate a Harness acceptance response against the request context.
///
/// Returns `Ok(ValidatedResponse)` if all checks pass.
/// Returns `Err` with a descriptive message on first failure.
pub fn validate_harness_response(
    response: &Value,
    ctx: &RequestContext,
) -> Result<ValidatedResponse, String> {
    let r = response.get("result").unwrap_or(response);

    // ── 1. Identity fields must match exactly ──
    eq(r, "hcr_id", &ctx.hcr_id)?;
    eq(r, "claim_id", &ctx.claim_id)?;
    eq(r, "run_id", &ctx.run_id)?;
    eq(r, "principal_id", &ctx.principal_id)?;
    eq(r, "gateway_session_id", &ctx.gateway_session_id)?;
    eq(r, "registry_snapshot_id", &ctx.registry_snapshot_id)?;
    eq(r, "operation", &ctx.operation)?;
    eq(r, "idempotency_key", &ctx.idempotency_key)?;

    // ── 2. Required fields must be non-empty and well-formatted ──
    let harness_execution_id = non_empty(r, "harness_execution_id")?;
    let candidate_id = non_empty(r, "candidate_id")?;
    let candidate_digest = valid_sha256(r, "candidate_digest")?;
    let evidence_digest = valid_sha256(r, "evidence_digest")?;
    let overall_outcome = valid_outcome(r)?;

    // ── 3. Gate results must exist ──
    let gates = r
        .get("gate_results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing gate_results".to_string())?;

    if gates.len() != 5 {
        return Err(format!("expected 5 gates, got {}", gates.len()));
    }

    // ── 4. Gate kinds must be unique ──
    let mut seen = HashSet::new();
    let expected = [
        "scaffold",
        "build",
        "trusted_test",
        "trusted_smoke",
        "artifact",
    ];
    for g in gates {
        let kind = g.get("gate_kind").and_then(|v| v.as_str()).unwrap_or("");
        if !expected.contains(&kind) {
            return Err(format!("unexpected gate_kind: {kind}"));
        }
        if !seen.insert(kind) {
            return Err(format!("duplicate gate_kind: {kind}"));
        }
    }

    // ── 5. Outcome consistency ──
    match overall_outcome {
        "CandidatePassed" => {
            if gates
                .iter()
                .any(|g| g.get("passed").and_then(|v| v.as_bool()) != Some(true))
            {
                return Err("CandidatePassed but some gates not passed".into());
            }
        }
        "CandidateFailed" => {
            if !gates
                .iter()
                .any(|g| g.get("is_candidate_failure").and_then(|v| v.as_bool()) == Some(true))
            {
                return Err("CandidateFailed but no candidate failure gate".into());
            }
        }
        "InfrastructureFailure" => {
            if !gates.iter().any(|g| {
                g.get("passed").and_then(|v| v.as_bool()) == Some(false)
                    && g.get("is_candidate_failure").and_then(|v| v.as_bool()) != Some(true)
            }) {
                return Err("InfrastructureFailure but no infra gate".into());
            }
        }
        _ => return Err(format!("unknown outcome: {overall_outcome}")),
    }

    // ── 6. Artifact digest (required for CandidatePassed) ──
    let artifact_digest = match overall_outcome {
        "CandidatePassed" => {
            let d = r
                .get("artifact_digest")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if d.is_empty() {
                return Err("CandidatePassed but missing artifact_digest".into());
            }
            if d == "verified" {
                return Err("artifact_digest is 'verified', not a real SHA-256".into());
            }
            validate_sha256_fmt(d)?;
            Some(d.to_string())
        }
        _ => None,
    };
    let component_manifest_digest = match overall_outcome {
        "CandidatePassed" => Some(valid_sha256(r, "component_manifest_digest")?.to_string()),
        _ => None,
    };
    let delivery_manifest_ref = match overall_outcome {
        "CandidatePassed" => {
            let d = non_empty(r, "delivery_manifest_ref")?;
            Some(d.to_string())
        }
        _ => None,
    };
    let delivery_manifest_digest = match overall_outcome {
        "CandidatePassed" => Some(valid_sha256(r, "delivery_manifest_digest")?.to_string()),
        _ => None,
    };

    // ── 7. Artifact ref must be a controlled relative path ──
    let artifact_ref = r
        .get("artifact_ref")
        .and_then(|v| v.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(art_ref) = artifact_ref.as_deref() {
        if !art_ref.is_empty() {
            if art_ref.starts_with("sha256:") {
                validate_sha256_fmt(art_ref)?;
            } else if art_ref.contains("..") || std::path::Path::new(art_ref).is_absolute() {
                return Err(format!("invalid artifact_ref: {art_ref}"));
            }
        }
    }

    // ── 8. Candidate digest format (already validated above) ──
    // harness_execution_id non-empty (already validated above)

    Ok(ValidatedResponse {
        harness_execution_id: harness_execution_id.to_string(),
        candidate_id: candidate_id.to_string(),
        overall_outcome: overall_outcome.to_string(),
        candidate_digest: candidate_digest.to_string(),
        artifact_ref,
        artifact_digest,
        component_manifest_digest,
        delivery_manifest_ref,
        delivery_manifest_digest,
        evidence_digest: evidence_digest.to_string(),
        gate_count: gates.len(),
    })
}

fn eq(v: &Value, key: &str, expected: &str) -> Result<(), String> {
    let actual = v.get(key).and_then(|v| v.as_str()).unwrap_or("");
    if actual != expected {
        return Err(format!("{key}: expected '{expected}', got '{actual}'"));
    }
    Ok(())
}

fn non_empty<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    let s = v.get(key).and_then(|v| v.as_str()).unwrap_or("");
    if s.is_empty() {
        return Err(format!("{key} is empty"));
    }
    Ok(s)
}

fn valid_sha256<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    let s = non_empty(v, key)?;
    validate_sha256_fmt(s)?;
    Ok(s)
}

fn validate_sha256_fmt(s: &str) -> Result<(), String> {
    if !s.starts_with("sha256:") {
        return Err(format!("{s} does not start with sha256:"));
    }
    let hex_part = &s[7..];
    if hex_part.len() != 64 || !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("{s} has invalid hex part"));
    }
    Ok(())
}

fn valid_outcome<'a>(v: &'a Value) -> Result<&'a str, String> {
    let s = non_empty(v, "overall_outcome")?;
    match s {
        "CandidatePassed" | "CandidateFailed" | "InfrastructureFailure" => Ok(s),
        _ => Err(format!("invalid outcome: {s}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> RequestContext {
        RequestContext {
            hcr_id: "hcr_test".into(),
            claim_id: "claim_test".into(),
            run_id: "run_test".into(),
            principal_id: "principal_test".into(),
            gateway_session_id: "session_test".into(),
            registry_snapshot_id: "snapshot_test".into(),
            operation: "external.coding_hcr_accept".into(),
            idempotency_key: "accept:test".into(),
        }
    }

    fn passed_response() -> Value {
        let ctx = context();
        json!({
            "result": {
                "hcr_id": ctx.hcr_id,
                "claim_id": ctx.claim_id,
                "run_id": ctx.run_id,
                "principal_id": ctx.principal_id,
                "gateway_session_id": ctx.gateway_session_id,
                "registry_snapshot_id": ctx.registry_snapshot_id,
                "operation": ctx.operation,
                "idempotency_key": ctx.idempotency_key,
                "harness_execution_id": "hex_test",
                "candidate_id": "candidate_test",
                "candidate_digest": format!("sha256:{}", "1".repeat(64)),
                "evidence_digest": format!("sha256:{}", "2".repeat(64)),
                "overall_outcome": "CandidatePassed",
                "artifact_ref": "candidate/target/release/component",
                "artifact_digest": format!("sha256:{}", "3".repeat(64)),
                "component_manifest_digest": format!("sha256:{}", "4".repeat(64)),
                "delivery_manifest_ref": "service_manifest_".to_string() + &"5".repeat(64),
                "delivery_manifest_digest": format!("sha256:{}", "6".repeat(64)),
                "gate_results": [
                    {"gate_kind":"scaffold", "passed":true},
                    {"gate_kind":"build", "passed":true},
                    {"gate_kind":"trusted_test", "passed":true},
                    {"gate_kind":"trusted_smoke", "passed":true},
                    {"gate_kind":"artifact", "passed":true}
                ]
            }
        })
    }

    #[test]
    fn passed_candidate_binds_a_real_component_manifest_digest() {
        let validated = validate_harness_response(&passed_response(), &context()).unwrap();
        assert_eq!(
            validated.component_manifest_digest.as_deref(),
            Some(format!("sha256:{}", "4".repeat(64)).as_str())
        );
    }

    #[test]
    fn passed_candidate_without_component_manifest_digest_is_rejected() {
        let mut response = passed_response();
        response["result"]
            .as_object_mut()
            .unwrap()
            .remove("component_manifest_digest");
        assert!(validate_harness_response(&response, &context())
            .unwrap_err()
            .contains("component_manifest_digest"));
    }

    #[test]
    fn passed_candidate_with_non_digest_component_manifest_is_rejected() {
        let mut response = passed_response();
        response["result"]["component_manifest_digest"] = json!("verified");
        assert!(validate_harness_response(&response, &context())
            .unwrap_err()
            .contains("does not start with sha256"));
    }
}
