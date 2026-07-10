//! Domain types for HarnessChangeRequest.
//!
//! A durable record stored in the `harness_change_requests` table.
//! Created by PR4A1 without a Run; PR4A2 consumes pending requests
//! and creates Runs.

use serde::{Deserialize, Serialize};

/// A durable HarnessChangeRequest record, stored in the
/// `harness_change_requests` table. Created by PR4A1 without a Run;
/// PR4A2 consumes pending requests and creates Runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessChangeRequest {
    pub request_id: String,
    pub source: String,
    pub source_message_id: String,
    pub session_id: String,
    pub principal_id: String,
    pub channel: String,
    pub chat_type: String,
    pub harness_id: String,
    pub requirement: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub run_id: Option<String>,
    pub error_code: Option<String>,
}
