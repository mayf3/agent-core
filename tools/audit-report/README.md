# Agent Core — Audit Report (read-only harness)

A standalone, read-only CLI that inspects a **copied** SQLite snapshot of the
Kernel journal and emits a human-readable `report.md` + machine-readable
`report.json`. It is the bridge from "solid Kernel" to "trustworthy audit" —
operators and CI can see runs, hash-chain integrity, unknown dispatches,
projection drift, undelivered ingress, approval waits, and duplicate-reply
safety without touching a running service.

**This tool lives outside `src/` and is not a Kernel dependency.** Long-term
extraction target: `agent-core-audit-tools`. See
`docs/external-audit-harness.md` for the design contract.

## Hard safety rules

- Reads only from an explicit `--db` SQLite path (a **copied** snapshot, never
  the live production DB).
- Opens the DB read-only (`SQLITE_OPEN_READONLY`).
- Refuses to run without `--db`.
- Does **not** read `.env`, `.agent-core`, logs, service config, secrets, or
  network resources.
- Does **not** mutate the Journal, approval state, connector state, or any DB.
- Does **not** stop or restart any service.

## Usage

```bash
# Copy a safe snapshot first (operator's responsibility):
cp /var/lib/agent-core/journal.db /tmp/audit-snapshot.db

node --experimental-strip-types tools/audit-report/cli.ts \
  --db /tmp/audit-snapshot.db \
  --out-dir ./out
#   optional: --health /path/to/health.json  --git-rev abc123
```

Writes `out/report.md` and `out/report.json`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | report written |
| 2 | `--db` missing |
| 3 | `--db` path is not a regular file |
| 4 | cannot open DB read-only (invalid/corrupt SQLite) |
| 5 | required tables missing |
| 6 | `--health` JSON malformed |
| 7 | cannot write output |

## Report sections

1. **Schema version** — expected vs actual (`schema_version` table or `PRAGMA user_version`).
2. **Hash-chain status** — recomputes `SHA-256(previous_hash | sequence | kind | payload_json)` per entry, mirroring `src/journal/hash_chain.rs`.
3. **Recent runs** — total, by-status breakdown, latest 10.
4. **Unknown dispatches** — `outbox_dispatches` where `status='unknown' AND acked_unknown=0`.
5. **Projection drift** — outbox rows whose terminal Journal fact (by `invocation_id`/`correlation_id`) disagrees with the projection `status` (mirrors the Kernel's `outbox_projection_drift_count` in `src/journal/queue_health.rs`).
6. **Undelivered ingress** — `IngressAccepted` events whose `payload.event_id` is present AND not found among the `correlation_id`s of any `SessionReady`/`RunStarted`/`RunCompleted`/`RunFailed` (mirrors `src/journal/recovery.rs` exactly — an `IngressAccepted` missing `payload.event_id` is not counted).
7. **Approval waits & expiries** — runs in `AwaitingApproval` + `ApprovalExpired` facts.
8. **Duplicate-reply safety** — `ingress_dedup` rows + idempotency-key collisions.

## Tests

```bash
node --test --experimental-strip-types tools/audit-report/test/audit.test.ts
```
