//! Read-only compatibility surface for historical HCR governance facts.
//!
//! The active HCR workflow is retired. This endpoint deliberately exposes no
//! mutation, recovery, resume, claim, gate, or settlement operation.

use super::write_json;
use crate::journal::JournalStore;
use anyhow::Result;
use serde_json::json;
use std::net::TcpStream;

pub(super) fn handle(stream: &mut TcpStream, journal: &JournalStore, path: &str) -> Result<()> {
    let hcr_id = path
        .strip_prefix("/v1/legacy/hcr/")
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    if hcr_id.is_empty() || hcr_id.contains('/') {
        return write_json(
            stream,
            404,
            json!({"ok":false,"error":"legacy_hcr_not_found"}),
        );
    }
    match journal.load_legacy_hcr_snapshot(hcr_id)? {
        Some(snapshot) => write_json(stream, 200, json!({"ok":true,"hcr":snapshot})),
        None => write_json(
            stream,
            404,
            json!({"ok":false,"error":"legacy_hcr_not_found"}),
        ),
    }
}
