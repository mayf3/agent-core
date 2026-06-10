CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  channel TEXT NOT NULL,
  conversation_key TEXT NOT NULL,
  summary TEXT,
  summarized_until_event_id TEXT,
  last_active_at TEXT NOT NULL,
  status TEXT NOT NULL,
  version INTEGER NOT NULL,
  UNIQUE(agent_id, channel, conversation_key)
);

CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  trigger_event_id TEXT NOT NULL,
  principal_json TEXT NOT NULL,
  parent_run_id TEXT,
  delegated_by TEXT,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS journal_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  run_id TEXT,
  session_id TEXT,
  correlation_id TEXT,
  kind TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  previous_hash TEXT,
  hash TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ingress_dedup (
  source TEXT NOT NULL,
  external_event_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  first_seen_at TEXT NOT NULL,
  PRIMARY KEY(source, external_event_id)
);

CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);
CREATE INDEX IF NOT EXISTS idx_journal_run_id ON journal_events(run_id);
CREATE INDEX IF NOT EXISTS idx_journal_session_id ON journal_events(session_id);
CREATE INDEX IF NOT EXISTS idx_journal_correlation_id ON journal_events(correlation_id);
CREATE INDEX IF NOT EXISTS idx_journal_kind ON journal_events(kind);
