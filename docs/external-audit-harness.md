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

The harness takes exactly these inputs, all passed explicitly as CLI arguments or
environment variables with safe defaults:

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
- Database snapshot: <path or hash>
- Health JSON: <present/absent>

## 1. Schema Version
- Expected: <version from Kernel constant or documentation>
- Actual: <version from pragma or schema_version table>
- Match: ✅ / ❌

## 2. Hash-Chain Status
- Total journal entries: <count>
- Linked entries (parent_uid resolves): <count>
- Broken links: <count>
- Integrity: ✅ / ❌ / Partial (see detail)

## 3. Recent Runs
- Total runs in snapshot: <count>
- Runs by status: pending=N, running=N, completed=N, failed=N, unknown=N
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
- Last dedupe key range: <from> – <to>
- Dedupe collisions found: <count>
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
    "linked_entries": 1498,
    "broken_links": 2,
    "integrity": "partial"
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
    "last_dedupe_key_range": ["run_001:msg_100", "run_001:msg_200"],
    "collisions": 0
  }
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

The Journal stores entries linked by `parent_uid`. The harness:

1. Counts total entries in the `journal_entries` table.
2. Counts entries where `parent_uid` is `NULL` (root entries, always valid).
3. Counts entries where `parent_uid` references an existing entry (linked).
4. Counts entries where `parent_uid` references a non-existent UID (broken link).

If `broken_links == 0`, integrity is `"ok"`. If `broken_links > 0`, integrity is
`"faulty"`. If the table cannot be read, integrity is `"unknown"`.

**Important**: This is a structural check, not a cryptographic one. The MVP does
not verify signatures or hashes of journal entry bodies. A future iteration may
add content-addressable verification.

### 5.3 Recent Runs

Query the `runs` table. Report total count and breakdown by `RunStatus`. List the
10 most recently created runs with their `id`, `status`, `created_at`, and
`updated_at`.

Purpose: give the operator a quick overview of what the system has processed.

### 5.4 Unknown Dispatches

Query the dispatching table for rows where the dispatch status is `unknown`.
These are dispatches that did not complete normally and may indicate stuck work
or system faults.

If the `/health` JSON was provided, cross-reference the `outbox_unknown_count`
field from health to ensure consistency with the raw DB query.

### 5.5 Projection Drift (Outbox)

Query outbox or projection tables to find rows where the projected/expected state
differs from the actual state. This detects corruption or inconsistency in the
projection rebuild mechanism.

If the `/health` JSON was provided, cross-reference `outbox_drift_count`.

### 5.6 Undelivered Ingress

Query the ingress/event table for rows whose delivery status is `undelivered`.
This indicates events that were received but not yet processed by the runtime.

### 5.7 Approval Waits & Expiries

Query the `runs` table for runs with `status = AwaitingApproval`. Count them and
find the oldest. Also query any approval-expiry audit log or flag to count
expired approvals.

If the `/health` JSON was provided, cross-reference `awaiting_approval_count`.

### 5.8 Duplicate-Reply Safety Notes

The Kernel deduplicates outbound messages using a dedupe key. The harness:

1. Queries the last range of dedupe keys used (e.g. `SELECT MIN(dedupe_key),
   MAX(dedupe_key) FROM outbox WHERE ...`).
2. Counts any collisions (duplicate dedupe keys with different payloads).

This is a safety indicator: a high collision count could mean duplicate replies
were sent to users.

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
| Required table missing (e.g. `journal_entries` does not exist) | Report that section as "table not found", continue with other sections, exit code 3 |
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
  hash-chain.mjs     # parent_uid link analysis
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
| Schema version | `schema_version` or `PRAGMA user_version` | `version` |
| Hash chain | `journal_entries` | `uid`, `parent_uid` |
| Recent runs | `runs` | `id`, `status`, `created_at`, `updated_at` |
| Unknown dispatches | `dispatching` or `outbox_dispatch` | `status`, `created_at` |
| Projection drift | `outbox_entries` or `projection_entries` | `projected_status`, `actual_status` |
| Undelivered ingress | `ingress_events` or `inbound_messages` | `delivery_status`, `created_at` |
| Approval waits | `runs` | `status = 'AwaitingApproval'` |
| Approval expiries | `runs` + `approval_audit_log` | `status = 'Failed'`, `failure_reason` |
| Dedupe safety | `outbox` or `deduplication_log` | `dedupe_key`, `payload_hash` |

This mapping will be validated and refined during implementation.
