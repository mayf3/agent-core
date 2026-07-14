//! event.observe.v0 — pull-based journal event observation API.
//!
//! ## Architecture
//!
//! ```text
//! Kernel Journal ──→ [safe read-only cursor pagination] ──→ External Harness
//! ```
//!
//! The Journal remains the single source of truth. Observe does NOT create a
//! second event table, does NOT modify the Journal, and does NOT ACK or delete
//! events. External consumers persist their own cursor.
//!
//! ## Hash-chain integrity
//!
//! Before serving any page the full chain is verified (`verify_hash_chain`).
//! On corruption the API returns `journal_corrupt` and serves NO events
//! (fail-closed).

use crate::journal::JournalStore;
use anyhow::{bail, ensure, Result};
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Schema version of the event.observe.v0 envelope. Incremented only when the
/// envelope shape changes in a backward-incompatible way.
pub const OBSERVE_SCHEMA_VERSION: &str = "event.observe.v0";

/// Maximum events per page (soft cap enforced server-side).
pub const MAX_OBSERVE_LIMIT: i64 = 1000;

/// Default events per page when the caller omits `limit`.
pub const DEFAULT_OBSERVE_LIMIT: i64 = 100;

// ---------------------------------------------------------------------------
// Query parameter type
// ---------------------------------------------------------------------------

/// Pull-based observe query parameters.
#[derive(Debug, Clone, Default)]
pub struct EventObserveQuery {
    /// Return events with `sequence >` this value. `None` or `0` means from
    /// the beginning of the Journal.
    pub after_sequence: Option<i64>,

    /// Maximum events to return (1..=MAX_OBSERVE_LIMIT).
    pub limit: i64,

    /// Optional exact-event-kind filter (the stored kind text, e.g.
    /// `"RunStarted"`). Empty string means no filter.
    pub event_kind: String,

    /// Optional exact-run-ID filter. Empty string means no filter.
    pub run_id: String,

    /// Optional exact-session-ID filter. Empty string means no filter.
    pub session_id: String,

    /// Optional principal-ID filter. Resolved via a LIKE on the `principal_json`
    /// column. Empty string means no filter.
    pub principal_id: String,
}

// ---------------------------------------------------------------------------
// Envelope types (event.observe.v0 contract)
// ---------------------------------------------------------------------------

/// A single observed event envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ObservedEvent {
    pub schema_version: &'static str,
    pub event_id: String,
    pub event_kind: String,
    pub occurred_at: String,
    pub agent_id: Option<String>,
    pub principal_id: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub correlation_id: Option<String>,
    pub payload: Value,
}

/// The observe page response.
#[derive(Debug, Clone, Serialize)]
pub struct EventObserveResponse {
    pub schema_version: &'static str,
    pub events: Vec<ObservedEvent>,
    pub next_cursor: i64,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Payload redaction
// ---------------------------------------------------------------------------

/// Recursively redact sensitive fields from a JSON payload.
///
/// Rules:
/// - Any object key whose lowercased name contains `token`, `secret`, `apikey`,
///   `api_key`, `password`, `passwd`, or equals `authorization`, `bearer`,
///   `private_key` → value replaced with `"[REDACTED]"`.
/// - Nested objects/arrays are traversed.
pub fn redact_payload(payload: &Value) -> Value {
    match payload {
        Value::Object(map) => {
            let mut result = serde_json::Map::new();
            for (k, v) in map {
                if is_sensitive_key(k) {
                    result.insert(k.clone(), Value::String("[REDACTED]".to_string()));
                } else {
                    result.insert(k.clone(), redact_payload(v));
                }
            }
            Value::Object(result)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(redact_payload).collect()),
        other => other.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("apikey")
        || lower.contains("api_key")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower == "authorization"
        || lower == "bearer"
        || lower == "private_key"
}

// ---------------------------------------------------------------------------
// JournalStore observe method
// ---------------------------------------------------------------------------

/// Column offsets for the LEFT JOIN query returned by `observe_query_sql`.
const COL_SEQUENCE: usize = 0;
const COL_EVENT_ID: usize = 1;
const COL_RUN_ID: usize = 2;
const COL_SESSION_ID: usize = 3;
const COL_CORRELATION_ID: usize = 4;
const COL_KIND: usize = 5;
const COL_PAYLOAD_JSON: usize = 6;
const COL_HASH: usize = 7;
const COL_CREATED_AT: usize = 8;
const COL_AGENT_ID: usize = 9;
const COL_PRINCIPAL_JSON: usize = 10;

/// Build the observe SQL with a LEFT JOIN on `runs`. Optional filters use an
/// empty-string sentinel pattern (`?N = '' OR col = ?N`) so the number of
/// parameters is always fixed.
fn observe_query_sql() -> &'static str {
    "SELECT je.sequence, je.event_id, je.run_id, je.session_id, je.correlation_id, \
            je.kind, je.payload_json, je.hash, je.created_at, \
            r.agent_id, r.principal_json \
     FROM journal_events je \
     LEFT JOIN runs r ON je.run_id = r.id \
     WHERE je.sequence > ?1 \
       AND (?2 = '' OR je.kind = ?2) \
       AND (?3 = '' OR je.run_id = ?3) \
       AND (?4 = '' OR je.session_id = ?4) \
     ORDER BY je.sequence ASC \
     LIMIT ?5"
}

impl JournalStore {
    /// Pull a page of journal events after the given cursor, applying optional
    /// filters. Returns envelopes that match the `event.observe.v0` contract.
    ///
    /// # Fail-closed on hash-chain corruption
    ///
    /// Before serving any page, the full hash chain is verified. If the chain
    /// is corrupt the method returns `bail!("journal_corrupt")`.
    ///
    /// # Principal-ID filtering
    ///
    /// Because `principal_id` lives inside `principal_json` (a JSON TEXT column)
    /// on the `runs` table, we apply this filter as a post-query step in Rust
    /// rather than in SQL. When `principal_id` is set, up to `limit` matching
    /// events are returned (potentially fewer than `limit` if the remaining
    /// events are for a different principal).
    pub fn observe_events(&self, query: &EventObserveQuery) -> Result<EventObserveResponse> {
        // ---- 1. Validate ----
        ensure!(
            query.limit >= 1 && query.limit <= MAX_OBSERVE_LIMIT,
            "invalid_limit"
        );
        let after_seq = query.after_sequence.unwrap_or(0);
        let fetch_limit = query.limit + 1; // +1 to detect has_more

        // ---- 2. Fail-closed: verify hash chain using STORED kind text ----
        // Unlike verify_hash_chain() which re-serialises through the enum
        // (and thus flags unknown/future-kind events as corrupt), our
        // verification uses the raw kind column.  This preserves future-kind
        // events while still detecting actual tampering (hash mismatch).
        if !self.verify_hash_chain_stored_kind()? {
            bail!("journal_corrupt");
        }

        // ---- 3. Query events + LEFT JOIN runs ----
        let raw_rows: Vec<RawObserveRow> = {
            let conn = self
                .conn
                .lock()
                .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;

            let mut stmt = conn.prepare(observe_query_sql())?;

            let param_kind = if query.event_kind.is_empty() {
                ""
            } else {
                &query.event_kind
            };
            let param_run = if query.run_id.is_empty() {
                ""
            } else {
                &query.run_id
            };
            let param_session = if query.session_id.is_empty() {
                ""
            } else {
                &query.session_id
            };

            let rows: Vec<RawObserveRow> = stmt
                .query_map(
                    rusqlite::params![after_seq, param_kind, param_run, param_session, fetch_limit],
                    |row| {
                        Ok(RawObserveRow {
                            sequence: row.get(COL_SEQUENCE)?,
                            event_id: row.get(COL_EVENT_ID)?,
                            run_id: row.get(COL_RUN_ID)?,
                            session_id: row.get(COL_SESSION_ID)?,
                            correlation_id: row.get(COL_CORRELATION_ID)?,
                            kind: row.get(COL_KIND)?,
                            payload_json: row.get(COL_PAYLOAD_JSON)?,
                            hash: row.get(COL_HASH)?,
                            created_at: row.get(COL_CREATED_AT)?,
                            agent_id: row.get(COL_AGENT_ID)?,
                            principal_json: row.get(COL_PRINCIPAL_JSON)?,
                        })
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            // stmt dropped here
            // conn lock dropped here
            rows
        };

        // ---- 4. Apply principal_id filter (Rust-level) ----
        let filter_principal = !query.principal_id.is_empty();
        let rows: Vec<RawObserveRow> = if filter_principal {
            let target = &query.principal_id;
            raw_rows
                .into_iter()
                .filter(|r| {
                    r.principal_json
                        .as_ref()
                        .map(|pj| {
                            // Parse principal_id from JSON (cheap for bounded rows)
                            serde_json::from_str::<serde_json::Value>(pj)
                                .ok()
                                .and_then(|v| v.get("principal_id")?.as_str().map(|s| s.to_string()))
                                .map(|pid| pid == *target)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
                })
                .collect()
        } else {
            raw_rows
        };

        // ---- 5. Detect has_more and trim to limit ----
        let has_more = rows.len() > query.limit as usize;
        let page: &[RawObserveRow] = if has_more {
            &rows[..query.limit as usize]
        } else {
            &rows[..]
        };

        // ---- 6. Build envelopes ----
        let next_cursor = if page.is_empty() {
            after_seq
        } else {
            page.last().unwrap().sequence
        };

        let mut events = Vec::with_capacity(page.len());
        for r in page {
            let payload: Value =
                serde_json::from_str(&r.payload_json).unwrap_or(serde_json::Value::Null);
            let principal_id: Option<String> = r
                .principal_json
                .as_ref()
                .and_then(|pj| {
                    serde_json::from_str::<serde_json::Value>(pj)
                        .ok()
                        .and_then(|v| v.get("principal_id")?.as_str().map(|s| s.to_string()))
                });

            events.push(ObservedEvent {
                schema_version: OBSERVE_SCHEMA_VERSION,
                event_id: r.event_id.clone(),
                event_kind: r.kind.clone(),
                occurred_at: r.created_at.clone(),
                agent_id: r.agent_id.clone(),
                principal_id,
                session_id: r.session_id.clone(),
                run_id: r.run_id.clone(),
                correlation_id: r.correlation_id.clone(),
                payload: redact_payload(&payload),
            });
        }

        Ok(EventObserveResponse {
            schema_version: OBSERVE_SCHEMA_VERSION,
            events,
            next_cursor,
            has_more,
        })
    }

    /// Verify the hash chain using the **stored** kind text from the database,
    /// rather than re-serialising through the enum (which would flag unknown /
    /// future-kind events as corrupt).
    ///
    /// This catches actual tampering (hash mismatch, broken previous_hash
    /// links) while preserving backward compatibility with future event kinds
    /// written by a newer kernel.
    fn verify_hash_chain_stored_kind(&self) -> Result<bool> {
        use crate::journal::hash_chain::event_hash;
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("journal mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "SELECT sequence, kind, payload_json, previous_hash, hash \
             FROM journal_events ORDER BY sequence",
        )?;
        let mut previous_hash: Option<String> = None;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (seq, kind_text, payload_json, stored_prev, stored_hash) = row?;
            let expected =
                event_hash(previous_hash.as_deref(), seq, &kind_text, &payload_json);
            if stored_prev != previous_hash || stored_hash != expected {
                return Ok(false);
            }
            previous_hash = Some(stored_hash);
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Raw row from the LEFT JOIN events × runs query.
struct RawObserveRow {
    sequence: i64,
    event_id: String,
    run_id: Option<String>,
    session_id: Option<String>,
    correlation_id: Option<String>,
    kind: String,
    payload_json: String,
    #[allow(dead_code)]
    hash: String,
    created_at: String,
    agent_id: Option<String>,
    principal_json: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use crate::journal::JournalStore;
    use chrono::Utc;
    use serde_json::json;

    // ---- Helpers ----

    fn seed_events(j: &JournalStore, n: usize) -> anyhow::Result<Vec<i64>> {
        let mut seqs = Vec::with_capacity(n);
        let session = SessionId("s_test".to_string());
        let run = RunId("r_test".to_string());

        for i in 0..n {
            let ev = j.append_event(
                JournalEventKind::RunStarted,
                Some(&run),
                Some(&session),
                Some(&format!("corr_{i}")),
                json!({"msg": format!("event_{i}"), "count": i}),
            )?;
            seqs.push(ev.sequence);
        }
        Ok(seqs)
    }

    // ---- Redaction ----

    #[test]
    fn redacts_sensitive_keys() {
        let payload = json!({
            "user_id": "abc",
            "openai_api_key": "sk-secret",
            "config": {
                "bearer": "tok_xyz",
                "normal": "hello"
            },
            "items": [{"secret": "hidden"}, {"safe": 1}]
        });
        let redacted = redact_payload(&payload);

        assert_eq!(redacted["user_id"].as_str(), Some("abc"));
        assert_eq!(redacted["openai_api_key"].as_str(), Some("[REDACTED]"));
        assert_eq!(redacted["config"]["bearer"].as_str(), Some("[REDACTED]"));
        assert_eq!(redacted["config"]["normal"].as_str(), Some("hello"));
        assert_eq!(redacted["items"][0]["secret"].as_str(), Some("[REDACTED]"));
        assert_eq!(redacted["items"][1]["safe"].as_i64(), Some(1));
    }

    // ---- observe_events happy path ----

    #[test]
    fn observe_returns_events_in_order() -> anyhow::Result<()> {
        let j = JournalStore::in_memory()?;
        let seqs = seed_events(&j, 5)?;

        let resp = j.observe_events(&EventObserveQuery {
            limit: 100,
            ..Default::default()
        })?;

        assert_eq!(resp.events.len(), 5);
        // event_id is a UUID, just check it's non-empty
        assert!(!resp.events[0].event_id.is_empty());
        assert!(resp.events[0].event_id.starts_with("event_"));
        assert!(!resp.has_more);
        assert_eq!(resp.next_cursor, seqs[4]);
        assert_eq!(resp.schema_version, OBSERVE_SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn observe_unknown_event_kind_is_preserved() -> anyhow::Result<()> {
        let j = JournalStore::in_memory()?;
        let run = RunId("r_u".to_string());
        let session = SessionId("s_u".to_string());

        // 1. Insert a known event (gets correct hash from append_event)
        let ev1 = j.append_event(
            JournalEventKind::RunStarted,
            Some(&run),
            Some(&session),
            None,
            json!({"msg": "known"}),
        )?;

        // 2. Insert a future-kind event WITH the CORRECT hash computed by
        //    the hash_chain module (accessible because this is a unit test).
        let future_seq = ev1.sequence + 1;
        let future_kind = "FutureKindFromFutureVersion";
        let future_payload = r#"{"msg":"future kind"}"#;
        let future_hash = crate::journal::hash_chain::event_hash(
            Some(&ev1.hash),
            future_seq,
            future_kind,
            future_payload,
        );

        j.execute_sql_for_test(&format!(
            "INSERT INTO journal_events \
             (sequence, event_id, run_id, session_id, kind, payload_json, \
              previous_hash, hash, created_at) \
             VALUES ({}, 'event_future_kind', '{}', '{}', '{}', '{}', '{}', '{}', '{}')",
            future_seq,
            run.0,
            session.0,
            future_kind,
            future_payload,
            ev1.hash,
            future_hash,
            Utc::now().to_rfc3339(),
        ))?;

        // 3. Observe — both events must be returned, and the future kind
        //    must have its original kind text preserved.
        let resp = j.observe_events(&EventObserveQuery {
            after_sequence: None,
            limit: 100,
            ..Default::default()
        })?;

        assert_eq!(
            resp.events.len(),
            2,
            "both known and future-kind events should be visible"
        );
        assert_eq!(resp.events[0].event_kind, "RunStarted");
        assert_eq!(
            resp.events[1].event_kind,
            "FutureKindFromFutureVersion",
            "stored kind text must be preserved, not re-routed via parse_kind"
        );

        // 4. The hash chain must still be intact (our stored-kind verification)
        assert!(
            j.verify_hash_chain_stored_kind()?,
            "stored-kind hash chain must be valid after future-kind insert"
        );

        Ok(())
    }
}
