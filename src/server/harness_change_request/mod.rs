//! HarnessChangeRequest endpoint — POST /v1/harness-change-requests.
//!
//! v0 (PR4A1): receives, authorizes, validates, deduplicates, and persists
//! HarnessChangeRequest records WITHOUT creating a Run or starting execution.
//! Returns a `pending` request_id.
//!
//! PR4A2 will consume pending requests, create Runs, and drive the scaffold.
//!
//! Authorization approach:
//! The handler independently validates the Feishu payload and re-checks
//! owner/p2p/Feishu using the same `is_coding_owner` check as the normal
//! Runtime flow.

use crate::config::KernelConfig;
use crate::domain::*;
use crate::gateway::Gateway;
use crate::journal::JournalStore;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::net::TcpStream;

/// Workspace ID pinned for HarnessChangeRequest Runs. (Deferred to PR4A2.)
#[allow(dead_code)]
pub const HARNESS_DEV_WORKSPACE_PINNING_DEFERRED_TO_PR4A2: &str = "harness-dev";
/// Maximum tool rounds for a HarnessChangeRequest Run. (Deferred to PR4A2.)
#[allow(dead_code)]
pub const HCR_MAX_TOOL_ROUNDS_DEFERRED_TO_PR4A2: usize = 24;

const MAX_HARNESS_ID_LEN: usize = 64;
const MAX_REQUIREMENT_LEN: usize = 8000;

/// Stable error categories returned to the caller.
pub const ERR_INVALID_HARNESS_ID: &str = "INVALID_HARNESS_ID";
pub const ERR_EMPTY_HARNESS_REQUIREMENT: &str = "EMPTY_HARNESS_REQUIREMENT";
pub const ERR_OWNER_REQUIRED: &str = "HARNESS_CHANGE_REQUEST_OWNER_REQUIRED";
pub const ERR_P2P_REQUIRED: &str = "HARNESS_CHANGE_REQUEST_P2P_REQUIRED";
pub const ERR_CHANNEL_REQUIRED: &str = "HARNESS_CHANGE_REQUEST_CHANNEL_REQUIRED";
pub const ERR_SESSION_NOT_FOUND: &str = "SESSION_NOT_FOUND";
pub const ERR_INVALID_SOURCE_MESSAGE_ID: &str = "INVALID_SOURCE_MESSAGE_ID";
pub const ERR_CONFLICT: &str = "HARNESS_CHANGE_REQUEST_CONFLICT";
pub const ERR_INTERNAL: &str = "HARNESS_CHANGE_REQUEST_INTERNAL_ERROR";

/// Stable, user-safe error prefix list for server/mod.rs routing.
pub const HCR_ERROR_CATEGORIES: &[&str] = &[
    ERR_INVALID_HARNESS_ID,
    ERR_EMPTY_HARNESS_REQUIREMENT,
    ERR_OWNER_REQUIRED,
    ERR_P2P_REQUIRED,
    ERR_CHANNEL_REQUIRED,
    ERR_SESSION_NOT_FOUND,
    ERR_INVALID_SOURCE_MESSAGE_ID,
    ERR_CONFLICT,
];

fn validate_harness_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id must not be empty");
    }
    if id.len() > MAX_HARNESS_ID_LEN {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id too long (max {MAX_HARNESS_ID_LEN})");
    }
    if id.starts_with('-') || id.ends_with('-') {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id must not start or end with hyphen");
    }
    if id.contains("--") {
        bail!("{ERR_INVALID_HARNESS_ID}: harness_id must not contain consecutive hyphens");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!(
            "{ERR_INVALID_HARNESS_ID}: harness_id must match [a-z0-9-] with no consecutive hyphens"
        );
    }
    Ok(())
}

fn validate_requirement(req: &str) -> Result<()> {
    let trimmed = req.trim();
    if trimmed.is_empty() {
        bail!("{ERR_EMPTY_HARNESS_REQUIREMENT}: requirement must not be empty");
    }
    if trimmed.len() > MAX_REQUIREMENT_LEN {
        bail!("{ERR_EMPTY_HARNESS_REQUIREMENT}: requirement too long (max {MAX_REQUIREMENT_LEN})");
    }
    Ok(())
}

fn extract_feishu_fields(payload: &Value) -> Result<(String, String, String, String)> {
    let sender_open_id = payload
        .get("sender_open_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chat_type = payload
        .get("chat_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chat_id = payload
        .get("chat_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sender_type = payload
        .get("sender_type")
        .and_then(|v| v.as_str())
        .unwrap_or("user")
        .to_string();

    if sender_type == "app" {
        bail!("{ERR_CHANNEL_REQUIRED}: bot sender not supported");
    }
    if sender_open_id.is_empty() {
        bail!("{ERR_INTERNAL}: missing sender_open_id in payload");
    }
    if chat_type.is_empty() {
        bail!("{ERR_INTERNAL}: missing chat_type in payload");
    }

    Ok((sender_open_id, chat_type, chat_id, sender_type))
}

/// Sanitize an error into a fixed category. Internal details never leaked.
pub fn sanitise_hcr_error(error: &anyhow::Error) -> &'static str {
    let msg = error.to_string();
    for cat in HCR_ERROR_CATEGORIES {
        if msg.starts_with(cat) {
            return cat;
        }
    }
    ERR_INTERNAL
}

/// Map an HCR error category to an HTTP status code.
fn hcr_error_to_http_status(cat: &str) -> u16 {
    match cat {
        ERR_INVALID_HARNESS_ID | ERR_EMPTY_HARNESS_REQUIREMENT | ERR_INVALID_SOURCE_MESSAGE_ID => {
            400
        }
        ERR_OWNER_REQUIRED | ERR_P2P_REQUIRED | ERR_CHANNEL_REQUIRED => 403,
        ERR_SESSION_NOT_FOUND => 404,
        ERR_CONFLICT => 409,
        _ => 500,
    }
}

/// Handle POST /v1/harness-change-requests.
///
/// The Connector intercepts the "创建 Harness" command and sends the original
/// Feishu webhook payload alongside the parsed fields. The Kernel independently
/// re-validates owner/p2p using existing checks, then persists the request.
pub fn handle(
    journal: &JournalStore,
    _gateway: &Gateway,
    config: &KernelConfig,
    body: &Value,
) -> Result<Value> {
    let harness_id = body
        .get("harness_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let requirement = body
        .get("requirement")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let source_message_id = body
        .get("source_message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    validate_harness_id(harness_id)?;
    validate_requirement(requirement)?;
    if source_message_id.is_empty() {
        bail!("{ERR_INVALID_SOURCE_MESSAGE_ID}: source_message_id is required");
    }

    let payload = body.get("payload").cloned().unwrap_or_default();
    let (sender_open_id, chat_type, _chat_id, _sender_type) = extract_feishu_fields(&payload)?;

    let principal = RunPrincipal {
        principal_id: PrincipalId(format!("feishu:open_id:{sender_open_id}")),
        subject: PrincipalSubject::FeishuOpenId(sender_open_id.clone()),
        source: PrincipalSource::Feishu,
        grants: vec![],
        requester_id: Some(format!("feishu:open_id:{sender_open_id}")),
    };

    let is_owner =
        crate::runtime::coding_grants::is_coding_owner(config, &principal, Some(&chat_type));
    if !is_owner {
        if chat_type != "p2p" {
            bail!("{ERR_P2P_REQUIRED}: HarnessChangeRequest is only supported in private chat");
        }
        bail!("{ERR_OWNER_REQUIRED}: only the configured coding owner can create harnesses");
    }

    let session_id = config.agent_id.0.clone();
    let (request_id, deduplicated) = journal.create_harness_change_request(
        "Feishu",
        source_message_id,
        &session_id,
        &principal.principal_id.0,
        "Feishu",
        &chat_type,
        harness_id,
        requirement,
    )?;

    Ok(serde_json::json!({
        "ok": true,
        "request_id": request_id,
        "status": "pending",
        "deduplicated": deduplicated,
    }))
}

/// HTTP-level handler for POST /v1/harness-change-requests.
///
/// Calls `handle()` and maps the result (success or typed error) to an HTTP
/// response on the given stream. This keeps the HTTP-wiring out of `mod.rs`.
pub fn handle_http(
    stream: &mut TcpStream,
    journal: &JournalStore,
    gateway: &Gateway,
    config: &KernelConfig,
    body: &Value,
) -> Result<()> {
    match handle(journal, gateway, config, body) {
        Ok(j) => super::write_json(stream, 200, j),
        Err(e) => {
            let cat = sanitise_hcr_error(&e);
            let status = hcr_error_to_http_status(cat);
            let msg = if status == 500 {
                eprintln!("HCR internal error: {:?}", e);
                ERR_INTERNAL
            } else {
                cat
            };
            super::write_json(stream, status, json!({"ok": false, "error": msg}))
        }
    }
}

#[cfg(test)]
mod tests;
