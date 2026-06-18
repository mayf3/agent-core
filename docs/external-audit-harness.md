# External Audit Harness — MVP Design

## 1. Purpose

The External Audit Harness produces a human-readable and machine-readable report of
what an Agent Core runtime has processed. It is the bridge from "solid Kernel" to
"trustworthy replay and self-evolution": before the user can trust automated replay
or evolution orchestration, they need a visible, auditable record of runs, state
health, hash-chain integrity, and outstanding approvals.

This document defines the **MVP** scope — the smallest useful read-only report
generator that meets the audit need without over-engineering.

## 2. Boundary & Ownership

**The harness is external to the Rust Kernel.** It is not a module, crate, or
dependency of the Kernel. It must never be imported by any `src/` code.

- Location in this repository (if incubated temporarily): `tools/audit-report/`
- Long-term target: a separate repository, e.g. `agent-core-audit-tools`
- Relationship to Kernel: read-only consumer of the Journal SQLite schema and
  `/health` endpoint semantics. The harness couples to the schema, not the code.

**Restrictions:**

- No Rust Kernel crate dependency. The harness should be a standalone script
  (Node.js or Python) that opens a copied SQLite file, runs `SELECT` queries,
  and writes `report.md` + `report.json`.
- No TypeScript/Feishu connector dependency.
- No workflow, multi-agent, shell, memory, dynamic hook, plugin registry,
  sandbox, or self-evolution logic.
- No mutation of the input database or any production data.

## 3. Inputs

The harness takes exactly these inputs, all passed explicitly as CLI arguments:

| Input | Required | Format | Description |
|---|---|---|---|
| `--db` | Yes | File path | Path to a **copied** SQLite database snapshot. The harness refuses to run if the path is empty, missing, or not a regular file. The user is responsible for creating a safe copy (e.g. `cp /var/lib/agent-core/journal.db /tmp/audit-snapshot.db`). |
| `--health` | No | File path | Path to a JSON file containing a `/health` endpoint response. Optional; if absent the report notes "health JSON not provided". |
| `--git-rev` | No | String | Git revision (commit hash, tag, or branch name) that identifies the version of the Kernel that produced the snapshot. If absent, the report notes "not specified". |

The harness must **refuse to run** if:
- `--db` is not provided, is empty, or points to a non-existent file.
- The file at `--db` is not a valid SQLite database (header check).

**What the harness does NOT read:**

- `.env`, `.openduck`, `.openclaw`, or any dotfile in the home directory or
  working directory.
- Log files (`.log`, `journalctl`, `syslog`).
- Production SQLite database (the user must copy it).
- `Authorization` headers, API keys, tokens, or any credential store.
- Any network socket or service endpoint (except a local `/health` JSON file
  that was pre-fetched by the user).
- `stdin` — the harness must be fully parameterized by CLI arguments.

## 4. Outputs

The harness produces two files in the specified output directory (default: current
working directory, overridable with `--out-dir`):

### 4.1 `report.md` — Human-Readable Report

A Markdown document intended for operator review, PR review, or attaching to a
support ticket. Structure:

```markdown
# Audit Report — <timestamp>

## Metadata
- Generated at: <ISO 8601>
- Git revision: <input or "not specified">
- Database snapshot: <path or file hash>
- Health JSON: <present/absent>

## 1. Schema Version
- Expected: <version from Kernel constant or documentation>
- Actual: <version from pragma or schema_version table>
- Match: ✅ / ❌

## 2. Hash-Chain Status
- Total journal entries: <count>
- Matching entries: <count>
- Mismatched entries (hash or previous_hash): <count>
- First failing sequence: <sequence> (or "none")
- Integrity: ✅ / ❌

## 3. Recent Runs
- Total runs in snapshot: <count>
- Runs by status: pending=N, running=N, completed=N, failed=N, unknown=N, awaiting_approval=N
- Most recent 10 runs (ID, status, created_at, updated_at)

## 4. Unknown Dispatches
- Dispatch rows with status=unknown: <count>
- Oldest unknown: <timestamp>
- Health context: <if health JSON provided, show dispatch-related fields>

## 5. Projection Drift (Outbox)
- Total outbox entries: <count>
- Entries with drift (projected != actual status): <count>
- Latest drift timestamp: <timestamp>
- Health context: <if health JSON provided, show drift fields>

## 6. Undelivered Ingress
- Ingress rows with status=undelivered: <count>
- Oldest undelivered: <timestamp>

## 7. Approval Waits & Expiries
- Runs waiting for approval: <count>
- Expired approvals: <count>
- Oldest pending approval: <timestamp>
- Approval TTL configured: <if available>

## 8. Duplicate-Reply Safety Notes
- Ingress dedup entries: <count>
- Oldest dedup entry: <timestamp>
- Idempotency collisions found: <count>
- Comment: <brief assessment>
```

### 4.2 `report.json` — Machine-Readable Report

A JSON document with the same data, suitable for automated processing (CI,
dashboards, alerting). Structure:

```jsonc
{
  "meta": {
    "generated_at": "2026-06-18T12:00:00Z",
    "git_revision": "abc123...",
    "db_snapshot_path": "/tmp/audit-snapshot.db",
    "health_json_provided": true
  },
  "schema_version": {
    "expected": 1,
    "actual": 1,
    "match": true
  },
  "hash_chain": {
    "total_entries": 1500,
    "matching_entries": 1498,
    "mismatched_entries": 2,
    "first_failing_sequence": 421,
    "integrity": "faulty"
  },
  "recent_runs": {
    "total": 200,
    "by_status": { "pending": 0, "running": 0, "completed": 190, "failed": 8, "unknown": 2 },
    "latest_10": [ /* ... */ ]
  },
  "unknown_dispatches": {
    "count": 2,
    "oldest_at": "2026-06-17T10:00:00Z"
  },
  "projection_drift": {
    "count": 1,
    "latest_at": "2026-06-17T14:00:00Z"
  },
  "undelivered_ingress": {
    "count": 0,
    "oldest_at": null
  },
  "approval": {
    "waiting_count": 1,
    "expired_count": 0,
    "oldest_pending_at": "2026-06-17T08:00:00Z"
  },
  "duplicate_reply_safety": {
    "ingress_dedup_count": 1200,
    "oldest_dedup_at": "2026-06-01T00:00:00Z",
    "idempotency_collisions": 0
  },
}
```

## 5. Required Report Sections — Detail

### 5.1 Schema Version

Read the schema version from either:
1. A `schema_version` table (`SELECT version FROM schema_version ORDER BY id DESC LIMIT 1`), or
2. The SQLite `PRAGMA user_version`.

Compare with the expected version documented in the Kernel (currently `1`). Report
match/mismatch.

Purpose: detect if the snapshot was produced by a different Kernel version than
expected, which could invalidate query assumptions.

### 5.2 Hash-Chain Status

The Journal stores entries in `journal_events` with an append-only hash chain.
Each entry carries the hash of the prior entry, forming a verifiable chain. The
hash is computed deterministically as:

```
SHA-256(previous_hash | sequence | kind | payload_json)
```

where `|` is literal `|` separator, matching `src/journal/hash_chain.rs`
`event_hash()`. The harness mirrors `src/journal/sqlite.rs` `verify_hash_chain`:

1. Counts total entries in `journal_events`.
2. Iterates entries in `sequence` order, recomputing `expected_hash` from
   `previous_hash`, `sequence`, `kind`, and `payload_json`.
3. Verifies that each entry's `previous_hash` matches the prior entry's `hash`,
   and its `hash` matches the recomputed value.

If all entries pass, integrity is `"ok"`. On first mismatch, integrity is
`"faulty"` and the report records the failing `sequence` and `event_id`. If the
table cannot be read, integrity is `"unknown"`.

**Note on entry 1 (sequence=1)**: `previous_hash` is `NULL` (not the empty
string). The `event_hash` function treats `None` as `""` internally, so the
harness must replicate that — pass `NULL`/`None` as the previous-hash input,
not a literal `"null"` or empty string, to match the Kernel's computation.

### 5.3 Recent Runs

Query the `runs` table. Report total count and breakdown by `RunStatus`. List the
10 most recently created runs with their `id`, `status`, `created_at`, and
`updated_at`.

Purpose: give the operator a quick overview of what the system has processed.

### 5.4 Unknown Dispatches

Query `outbox_dispatches` for rows where `status = 'unknown'` and
`acked_unknown = 0`. These are dispatches that reached a terminal-unknown state
and have **not** been acknowledged by an operator (matching
`src/journal/queue_health.rs` `outbox_unknown_unacked_count`). An acknowledged
unknown (`acked_unknown = 1`) is accepted by the operator and no longer counts
as a health concern.

If the `/health` JSON was provided, cross-reference the `outbox_unknown_count`
field from health to ensure consistency with the raw DB query. The Kernel's
health endpoint reports the unacknowledged count.

### 5.5 Projection Drift (Outbox)

The Kernel maintains `outbox_dispatches` as a projection of the Journal's
terminal facts. A row "drifts" when the Journal already has a terminal event
(`ReceiptReceived` or `OutboxDispatchUnknown`) for the same invocation, but the
projection status disagrees. The harness mirrors
`src/journal/queue_health.rs` `outbox_projection_drift_count`:

1. Join `outbox_dispatches od` to `journal_events je` on
   `je.correlation_id = od.invocation_id`.
2. Find rows where `je.kind` is `ReceiptReceived` or `OutboxDispatchUnknown`
   (Journal has a terminal fact).
3. Check that the projection status matches — e.g. `od.status = 'succeeded'`
   with a Journal `ReceiptReceived` whose payload contains `"status":"Succeeded"`,
   or `od.status = 'failed'` with `"status":"Failed"`, or `od.status = 'unknown'`
   paired with `OutboxDispatchUnknown`.
4. Count rows where no matching terminal-status pair is found.

If the `/health` JSON was provided, cross-reference `outbox_drift_count`.

### 5.6 Undelivered Ingress

The Kernel does not store a `delivery_status` column. Instead, undelivered
ingress is derived from `journal_events`: an `IngressAccepted` event whose
`correlation_id` (which holds the payload `event_id`) has no matching
`SessionReady`, `RunStarted`, `RunCompleted`, or `RunFailed` event with the
same `correlation_id`. This matches `src/journal/recovery.rs`
`undelivered_ingress_events()` — `RunFailed` is included on purpose so that a
failed worker delivery is NOT re-queued on restart.

The harness:
1. Collects all `IngressAccepted` events.
2. Collects all `correlation_id` values from events of kind `SessionReady`,
   `RunStarted`, `RunCompleted`, `RunFailed`.
3. Filters to `IngressAccepted` events whose payload `event_id` is NOT in the
   delivered set.
4. Returns the count and oldest such event.

### 5.7 Approval Waits & Expiries

Query the `runs` table for runs with `status = 'AwaitingApproval'`. Count them
and find the oldest `created_at`. Expired approvals are runs that transitioned
from `AwaitingApproval` to `Failed` (the Kernel's approval-expiry sweep sets
the run status to `'Failed'` with a failure context indicating
`ApprovalExpired`). The harness:

1. Counts `runs` with `status = 'AwaitingApproval'`.
2. Queries `journal_events` for `ApprovalExpired` kind events or runs where
   `status = 'Failed'` and the payload/reason suggests approval expiry.
3. Reports both counts and the oldest pending approval timestamp.

If the `/health` JSON was provided, cross-reference `awaiting_approval_count`.

### 5.8 Duplicate-Reply Safety Notes

The Kernel deduplicates ingress events using the `ingress_dedup` table. On each
incoming event, the gateway calls `reserve_ingress(source, external_event_id)`
which inserts into `ingress_dedup` with `INSERT OR IGNORE`. If the row already
exists the event is rejected as a duplicate (`bail!("skip:duplicate_ingress")`).
The outbox meanwhile uses `idempotency_key` (unique constraint on
`outbox_dispatches.invocation_id`) to prevent duplicate dispatch of the same
intent.

The harness:
1. Queries the total count and time range of `ingress_dedup` entries.
2. Checks for any `outbox_dispatches` rows sharing the same `idempotency_key`
   with different `arguments_json` (potential duplicate with different payload).
3. Reports the count and the recency of the oldest dedup entry as a safety
   indicator — a stale dedup range does not directly imply a safety problem, but
   an unexpectedly small `ingress_dedup` footprint for a long-running system may
   warrant investigation.

## 6. Constraints & Safety Rules

| Rule | Enforcement |
|---|---|
| No mutation of input database | Open SQLite in read-only mode (`SQLITE_OPEN_READONLY`). Do not issue INSERT/UPDATE/DELETE. Verify required tables exist but do not create them. |
| No mutation of any other state | The harness writes only to `report.md` and `report.json` in the output directory. |
| No credential access | Do not read `.env`, `.openduck`, `.openclaw`, `~/.aws`, `~/.ssh`, `Authorization` headers, or any file matching secret patterns. |
| No network access | Do not make HTTP requests (except the user may pre-fetch `/health` and pass it as a file). |
| No service control | Do not stop, restart, or send signals to any process. |
| No implicit inputs | All inputs must be explicit CLI arguments. Refuse to run if required inputs are missing. |
| Graceful degradation | If a table is absent or empty, report that section as "not available" or "0" rather than crashing. |

## 7. Failure Modes

| Condition | Behaviour |
|---|---|
| `--db` not provided | Print error to stderr, exit code 1 |
| `--db` file does not exist | Print error to stderr, exit code 1 |
| `--db` file is not SQLite (no `SQLite format 3\000` header) | Print error to stderr, exit code 2 |
| Required table missing (e.g. `journal_events` does not exist) | Report that section as "table not found", continue with other sections, exit code 3 |
| `--health` file not valid JSON | Print warning to stderr, report "health JSON malformed", continue without cross-referencing, exit code 0 |
| Output directory not writable | Print error to stderr, exit code 4 |
| SQLite file is encrypted or corrupted | Catch `SQLITE_NOTADB` error, print message to stderr, exit code 5 |
| Empty tables | Report zero counts, no error |
| All tables missing | Report all sections as unavailable, exit code 3 |

## 8. Implementation Plan (MVP)

### Step 1 — Scaffold

```
mkdir -p tools/audit-report
```

Create:
- `tools/audit-report/index.mjs` — Main entry point
- `tools/audit-report/package.json` — Dependencies (better-sqlite3 or better-sqlite3 for Node.js bindings)
- `tools/audit-report/README.md` — Usage and extraction notice

### Step 2 — Core Query Modules

Each report section maps to one module:

```
tools/audit-report/
  index.mjs          # CLI argument parsing, orchestration
  schema-version.mjs # PRAGMA user_version / schema_version table
  hash-chain.mjs     # verify_hash_chain replay (sequence, previous_hash, hash)
  runs.mjs           # Recent runs summary
  dispatches.mjs     # Unknown dispatches
  drift.mjs          # Projection drift
  ingress.mjs        # Undelivered ingress
  approval.mjs       # Approval waits and expiries
  dedupe.mjs         # Duplicate-reply safety
  report.mjs         # report.md + report.json writer
```

### Step 3 — Integration

- Parse `--db`, `--health`, `--git-rev`, `--out-dir` from CLI.
- Open SQLite in read-only mode.
- If `--health` provided, load and parse JSON.
- Run each query module, collect results.
- Write `report.md` and `report.json` to the output directory.
- Exit with appropriate status code.

### Step 4 — Test Fixtures

- Create `tools/audit-report/fixtures/` with small, synthetic SQLite databases
  covering:
  - Healthy snapshot (all tables, no anomalies)
  - Broken hash chain snapshot
  - Empty snapshot (no tables)
  - Snapshot with unknown dispatches and drift
  - Snapshot with approval waits and expiries
- Test each module against each fixture.
- Run check-structure and check-local-secret-leaks after each change.

### Step 5 — Extraction Preparation

Document the extraction target in `tools/audit-report/README.md`:

```
Extraction target: agent-core-audit-tools
Reason: This is a read-only report generator. It couples to the Journal schema,
        not the Kernel code. It should live in a separate repository so that
        operator tooling does not depend on the Kernel build.
Removal: When extracted, delete tools/audit-report/ and update
         docs/external-audit-harness.md to point to the new repo.
```

## 9. Non-Goals (MVP)

The following are explicitly excluded from the MVP:

- Cryptographic hash-chain verification (content hashing, Merkle proofs).
- Replay runner or fixture evaluation.
- Self-evolution orchestration.
- Interactive dashboard or web UI.
- Real-time monitoring or alerting.
- Mutation of production state (approval, journal, connector state).
- Workflow, multi-agent, shell, memory, plugin, sandbox.

## 10. Extraction Target

Once the harness proves useful, move it to:

```
Repository: agent-core-audit-tools
Location:   agent-core-audit-tools/report/
```

The extraction preserves all query logic; only the repository layout and CI
pipeline change. The harness remains read-only and SQLite-coupled.

## 11. Appendix: Schema Mapping

Below is a mapping of report sections to likely SQLite tables in the Journal DB.
This is a design reference; the actual implementation must verify table existence.

| Section | Table(s) | Key Columns |
|---|---|---|
| Schema version | `PRAGMA user_version` | N/A (scalar) |
| Hash chain | `journal_events` | `sequence`, `previous_hash`, `hash`, `kind`, `payload_json` |
| Recent runs | `runs` | `id`, `status`, `created_at`, `updated_at` |
| Unknown dispatches | `outbox_dispatches` | `status`, `acked_unknown`, `created_at` |
| Projection drift | `outbox_dispatches` + `journal_events` | `od.invocation_id`, `je.correlation_id`, `od.status`, `je.kind`, `je.payload_json` |
| Undelivered ingress | `journal_events` | `kind = 'IngressAccepted'`, `correlation_id`, payload `event_id` |
| Approval waits | `runs` | `status = 'AwaitingApproval'`, `created_at` |
| Approval expiries | `runs` + `journal_events` | `status = 'Failed'`, `kind = 'ApprovalExpired'` or payload indicator |
| Dedupe safety | `ingress_dedup` + `outbox_dispatches` | `ingress_dedup.source`, `external_event_id`, `event_id`; `outbox_dispatches.idempotency_key`, `arguments_json` |

This mapping will be validated and refined during implementation.
