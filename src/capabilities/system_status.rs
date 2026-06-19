//! `system.status` — read-only system health and projection summary.
//!
//! Queries the Journal for aggregate health counts and returns them as a
//! structured JSON value. **Never returns secrets, payloads, tokens, or
//! raw event content** — only aggregate numbers and a rollup status string.
//!
//! This is a pure mechanical journal query — no product logic, no keyword
//! detection, no hardcoded human-facing format. The model receives the
//! structured result in its context (via the ReceiptReceived fact) and
//! decides how to format the user-facing reply (which goes through the
//! normal outbox → dispatcher → connector path).
//!
//! Temporary dogfood attribution: `system.status` is granted via
//! `KernelConfig::extra_allowed_operations` in the default config
//! (`common::test_config` and `KernelConfig::from_cli`), NOT by
//! `ExecutionProfile::for_channel`. This makes the grant a per-Agent
//! configuration choice, not a channel-level permission. Future agents
//! must explicitly configure the grant.

use crate::journal::JournalStore;
use serde_json::json;

pub fn execute(journal: &JournalStore) -> serde_json::Value {
    let h = |v: Result<i64, _>| v.unwrap_or(0);
    let hash_ok = journal.verify_hash_chain().ok().unwrap_or(false);
    let pending = h(journal.outbox_status_count(crate::domain::OutboxDispatchStatus::Pending));
    let unknown = h(journal.outbox_unknown_unacked_count());
    let dispatching = h(journal.outbox_status_count(crate::domain::OutboxDispatchStatus::Dispatching));
    let drift = h(journal.outbox_projection_drift_count());
    let undelivered = journal.undelivered_ingress_events().ok().map(|v| v.len() as i64).unwrap_or(0);
    let awaiting_approval = h(journal.awaiting_approval_count());
    let event_count = h(journal.event_count());
    let stale_dispatching = h(journal.outbox_stale_dispatching_count());
    let rollup = if !hash_ok {
        "corrupt"
    } else if unknown > 0 || drift > 0 || undelivered > 0 {
        "degraded"
    } else {
        "ok"
    };
    json!({
        "status": rollup,
        "hash_chain_ok": hash_ok,
        "event_count": event_count,
        "outbox": {
            "pending": pending,
            "dispatching": dispatching,
            "unknown_unacked": unknown,
            "stale_dispatching": stale_dispatching,
            "projection_drift": drift,
        },
        "ingress": { "undelivered": undelivered },
        "approval": { "awaiting": awaiting_approval },
        "summary": format!("status={} events={} pending={} drift={} undelivered={}", rollup, event_count, pending, drift, undelivered),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::JournalStore;

    #[test]
    fn fresh_journal_returns_status_ok() {
        let journal = JournalStore::in_memory().unwrap();
        let output = execute(&journal);
        assert_eq!(output["status"], "ok");
        assert_eq!(output["hash_chain_ok"].as_bool(), Some(true));
    }

    #[test]
    fn returns_aggregate_counts_not_secrets() {
        let journal = JournalStore::in_memory().unwrap();
        let output = execute(&journal);
        // Only aggregate numbers — no token, secret, or payload fields.
        assert!(output["event_count"].is_number());
        assert!(output["outbox"]["pending"].is_number());
        assert!(output["ingress"]["undelivered"].is_number());
        assert!(output.get("authorization").is_none());
        assert!(output.get("token").is_none());
        assert!(output.get("app_secret").is_none());
        assert!(output.get("raw_payload").is_none());
    }
}
