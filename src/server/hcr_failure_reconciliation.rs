//! Authenticated operator entry point for evidence-bound HCR failure recovery.

use crate::journal::JournalStore;
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

pub fn handle(journal: &JournalStore, hcr_id: &str, body: &Value) -> Result<Value> {
    let object = body
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("HCR_FAILURE_BODY_NOT_OBJECT"))?;
    reject_unknown_fields(object)?;
    let claim_id = required_string(object, "claim_id")?;
    let run_id = required_string(object, "run_id")?;
    let failure_event_id = required_string(object, "failure_event_id")?;
    if required_string(object, "expected_terminal")? != "failed" {
        bail!("HCR_FAILURE_EXPECTED_TERMINAL_INVALID");
    }

    let result = journal.reconcile_hcr_failure(hcr_id, claim_id, run_id, failure_event_id)?;
    Ok(json!({
        "ok": true,
        "status": "failed",
        "settlement_id": result.settlement_id,
        "hcr_id": result.hcr_id,
        "claim_id": result.claim_id,
        "run_id": result.run_id,
        "failure_evidence_event_id": result.failure_evidence_event_id,
        "idempotent": result.idempotent,
    }))
}

fn reject_unknown_fields(object: &Map<String, Value>) -> Result<()> {
    const ALLOWED: [&str; 4] = [
        "claim_id",
        "run_id",
        "failure_event_id",
        "expected_terminal",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        bail!("HCR_FAILURE_UNKNOWN_FIELD");
    }
    Ok(())
}

fn required_string<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("HCR_FAILURE_MISSING_{name}"))
}
