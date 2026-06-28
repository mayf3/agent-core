use rusqlite::params;
use sha2::{Digest, Sha256};

pub fn event_hash(previous_hash: Option<&str>, sequence: i64, kind: &str, payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(previous_hash.unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(sequence.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(kind.as_bytes());
    hasher.update(b"|");
    hasher.update(payload.as_bytes());
    hex::encode(hasher.finalize())
}

/// Append a journal event inside an existing SQLite transaction. Returns the
/// inserted event's sequence number. Used by control-plane operations that
/// need to atomically commit state + Journal facts in the same transaction.
pub fn append_event_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    kind: &str,
    payload_json: &str,
    created_at: &str,
) -> Result<i64, rusqlite::Error> {
    let previous: Option<(i64, String)> = tx
        .query_row(
            "SELECT sequence, hash FROM journal_events ORDER BY sequence DESC LIMIT 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .ok();
    let sequence = previous.as_ref().map(|(seq, _)| seq + 1).unwrap_or(1);
    let previous_hash = previous.map(|(_, hash)| hash);
    let hash = event_hash(previous_hash.as_deref(), sequence, kind, payload_json);
    tx.execute(
        "INSERT INTO journal_events
         (sequence, event_id, run_id, session_id, correlation_id, kind, payload_json, previous_hash, hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            sequence,
            format!("evt_{sequence}_{kind}"),
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            kind,
            payload_json,
            previous_hash,
            hash,
            created_at,
        ],
    )?;
    Ok(sequence)
}
