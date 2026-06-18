#!/usr/bin/env node
/**
 * External Audit Report — read-only harness (Agent Core).
 *
 * Reads a COPIED SQLite snapshot of the Kernel journal and emits a human-
 * readable report.md + machine-readable report.json. NEVER mutates the DB,
 * NEVER reads secrets / .env / production DB / logs, NEVER controls a service.
 *
 * See docs/external-audit-harness.md for the design contract this implements.
 *
 * Usage:
 *   node --experimental-strip-types cli.ts --db /tmp/snapshot.db [--health health.json] [--git-rev abc123] [--out-dir ./out]
 */

import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { execSync } from "node:child_process";

const EXPECTED_SCHEMA_VERSION = 1;
const EXPECTED_DELIVERED_KINDS = new Set([
  "SessionReady",
  "RunStarted",
  "RunCompleted",
  "RunFailed",
]);

interface Args {
  db: string;
  health?: string;
  gitRev?: string;
  outDir: string;
}

function parseArgs(argv: string[]): Args {
  const a: Record<string, string> = {};
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i];
    if (k === "--db" || k === "--health" || k === "--git-rev" || k === "--out-dir") {
      a[k.slice(2)] = argv[++i];
    }
  }
  const db = a["db"];
  if (!db) {
    console.error("error: --db is required (path to a copied SQLite snapshot)");
    process.exit(2);
  }
  if (!existsSync(db) || !statSync(db).isFile()) {
    console.error(`error: --db path is not a regular file: ${db}`);
    process.exit(3);
  }
  return {
    db,
    health: a["health"],
    gitRev: a["git-rev"],
    outDir: a["out-dir"] ?? process.cwd(),
  };
}

// Failure exit codes (design §7).
const EXIT_INVALID_DB = 4;
const EXIT_MISSING_TABLES = 5;
const EXIT_BAD_HEALTH = 6;
const EXIT_UNWRITABLE = 7;

function openReadOnly(path: string): DatabaseSync {
  try {
    return new DatabaseSync(path, { readOnly: true });
  } catch (e) {
    console.error(`error: cannot open SQLite DB read-only: ${(e as Error).message}`);
    process.exit(EXIT_INVALID_DB);
  }
}

function hasTable(db: DatabaseSync, name: string): boolean {
  const row = db.prepare(
    "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
  ).get(name) as { name?: string } | undefined;
  return row?.name === name;
}

function requireTables(db: DatabaseSync, names: string[]): void {
  const missing = names.filter((n) => !hasTable(db, n));
  if (missing.length > 0) {
    console.error(`error: required tables missing: ${missing.join(", ")}`);
    process.exit(EXIT_MISSING_TABLES);
  }
}

// --- Report section implementations ---

function schemaVersion(db: DatabaseSync) {
  let actual: number | null = null;
  if (hasTable(db, "schema_version")) {
    const row = db.prepare(
      "SELECT version FROM schema_version ORDER BY id DESC LIMIT 1",
    ).get() as { version?: number } | undefined;
    actual = row?.version ?? null;
  }
  if (actual === null) {
    const row = db.prepare("PRAGMA user_version").get() as { user_version?: number } | undefined;
    actual = row?.user_version ?? 0;
  }
  return {
    expected: EXPECTED_SCHEMA_VERSION,
    actual,
    match: actual === EXPECTED_SCHEMA_VERSION,
  };
}

function eventHash(
  previousHash: string | null,
  sequence: number,
  kind: string,
  payload: string,
): string {
  const h = createHash("sha256");
  h.update((previousHash ?? "") + "|");
  h.update(String(sequence) + "|");
  h.update(kind + "|");
  h.update(payload);
  return h.digest("hex");
}

function hashChain(db: DatabaseSync) {
  const rows = db.prepare(
    "SELECT sequence, event_id, kind, payload_json, previous_hash, hash FROM journal_events ORDER BY sequence ASC",
  ).all() as {
    sequence: number;
    event_id: string;
    kind: string;
    payload_json: string;
    previous_hash: string | null;
    hash: string;
  }[];
  let matching = 0;
  let firstFail: number | null = null;
  let prevHash: string | null = null;
  for (const r of rows) {
    const expected = eventHash(r.previous_hash, r.sequence, r.kind, r.payload_json);
    const prevOk = r.sequence === 1 ? r.previous_hash === null : r.previous_hash === prevHash;
    const hashOk = r.hash === expected;
    if (prevOk && hashOk) {
      matching++;
    } else if (firstFail === null) {
      firstFail = r.sequence;
    }
    prevHash = r.hash;
  }
  return {
    total_entries: rows.length,
    matching_entries: matching,
    mismatched_entries: rows.length - matching,
    first_failing_sequence: firstFail,
    integrity: rows.length === matching ? "ok" : "faulty",
  };
}

function recentRuns(db: DatabaseSync) {
  const byStatus: Record<string, number> = {};
  const totalRow = db.prepare("SELECT COUNT(*) AS c FROM runs").get() as { c: number };
  const groups = db.prepare(
    "SELECT status, COUNT(*) AS c FROM runs GROUP BY status",
  ).all() as { status: string; c: number }[];
  for (const g of groups) byStatus[g.status] = g.c;
  const latest = db.prepare(
    "SELECT id, session_id, status, created_at, updated_at FROM runs ORDER BY created_at DESC LIMIT 10",
  ).all() as {
    id: string;
    session_id: string;
    status: string;
    created_at: string;
    updated_at: string;
  }[];
  return { total: totalRow.c, by_status: byStatus, latest_10: latest };
}

function unknownDispatches(db: DatabaseSync) {
  const row = db.prepare(
    "SELECT COUNT(*) AS c, MIN(created_at) AS oldest FROM outbox_dispatches WHERE status='unknown' AND acked_unknown=0",
  ).get() as { c: number; oldest: string | null };
  return { count: row.c, oldest_at: row.oldest ?? null };
}

function projectionDrift(db: DatabaseSync) {
  // Mirror the Rust Kernel's outbox_projection_drift_count (src/journal/queue_health.rs).
  // Count outbox rows where a terminal journal fact exists (ReceiptReceived or
  // OutboxDispatchUnknown) for the same invocation_id (correlation_id), but the
  // projection status disagrees with the terminal fact.
  const row = db.prepare(`
    SELECT COUNT(*) AS c, MAX(od.updated_at) AS latest
    FROM outbox_dispatches od
    WHERE EXISTS (
      SELECT 1 FROM journal_events je
      WHERE je.correlation_id = od.invocation_id
        AND je.kind IN ('ReceiptReceived', 'OutboxDispatchUnknown')
    )
    AND NOT (
      (od.status = 'succeeded' AND EXISTS (
        SELECT 1 FROM journal_events je
        WHERE je.correlation_id = od.invocation_id
          AND je.kind = 'ReceiptReceived'
          AND je.payload_json LIKE '%"status":"Succeeded"%'
      ))
      OR (od.status = 'failed' AND EXISTS (
        SELECT 1 FROM journal_events je
        WHERE je.correlation_id = od.invocation_id
          AND je.kind = 'ReceiptReceived'
          AND je.payload_json LIKE '%"status":"Failed"%'
      ))
      OR (od.status = 'unknown' AND EXISTS (
        SELECT 1 FROM journal_events je
        WHERE je.correlation_id = od.invocation_id
          AND je.kind = 'OutboxDispatchUnknown'
      ))
    )
  `).get() as { c: number; latest: string | null };
  return { count: row.c, latest_at: row.latest ?? null };
}

function undeliveredIngress(db: DatabaseSync) {
  // Mirror the Rust Kernel's undelivered_ingress_events (src/journal/recovery.rs).
  // Count IngressAccepted events that have NO following terminal delivery event
  // (SessionReady/RunStarted/RunCompleted/RunFailed) sharing the same correlation_id.
  const row = db.prepare(`
    SELECT COUNT(*) AS c,
           MIN(e.created_at) AS oldest
    FROM journal_events e
    WHERE e.kind = 'IngressAccepted'
      AND NOT EXISTS (
        SELECT 1 FROM journal_events d
        WHERE d.correlation_id = e.correlation_id
          AND d.kind IN ('SessionReady', 'RunStarted', 'RunCompleted', 'RunFailed')
      )
  `).get() as { c: number; oldest: string | null };
  return { count: row.c, oldest_at: row.oldest ?? null };
}

function approval(db: DatabaseSync) {
  const waiting = db.prepare(
    "SELECT COUNT(*) AS c, MIN(created_at) AS oldest FROM runs WHERE status='AwaitingApproval'",
  ).get() as { c: number; oldest: string | null };
  const expired = db.prepare(
    "SELECT COUNT(*) AS c FROM journal_events WHERE kind='ApprovalExpired'",
  ).get() as { c: number };
  return {
    waiting_count: waiting.c,
    expired_count: expired.c,
    oldest_pending_at: waiting.oldest ?? null,
  };
}

function duplicateReplySafety(db: DatabaseSync) {
  const dedup = db.prepare(
    "SELECT COUNT(*) AS c, MIN(first_seen_at) AS oldest FROM ingress_dedup",
  ).get() as { c: number; oldest: string | null };
  // Idempotency-key collisions: an outbox dispatch's idempotency_key should be
  // unique per invocation. Count distinct invocations sharing a key.
  const collisions = db.prepare(
    `SELECT COUNT(*) AS c FROM (
       SELECT idempotency_key FROM outbox_dispatches
       WHERE idempotency_key IS NOT NULL AND idempotency_key <> ''
       GROUP BY idempotency_key HAVING COUNT(DISTINCT invocation_id) > 1
     )`,
  ).get() as { c: number };
  return {
    ingress_dedup_count: dedup.c,
    oldest_dedup_at: dedup.oldest ?? null,
    idempotency_collisions: collisions.c,
  };
}

// --- Output writers ---

function writeReports(report: any, outDir: string) {
  try {
    const dir = resolve(outDir);
    mkdirSync(dir, { recursive: true });
    const json = JSON.stringify(report, null, 2) + "\n";
    writeFileSync(join(dir, "report.json"), json);
    writeFileSync(join(dir, "report.md"), toMarkdown(report));
  } catch (e) {
    console.error(`error: cannot write reports: ${(e as Error).message}`);
    process.exit(EXIT_UNWRITABLE);
  }
}

function toMarkdown(r: any): string {
  const lines: string[] = [];
  lines.push("# Agent Core — Audit Report");
  lines.push("");
  lines.push(`Generated: ${r.meta.generated_at}`);
  lines.push(`Git revision: ${r.meta.git_revision ?? "not specified"}`);
  lines.push(`DB snapshot: ${r.meta.db_snapshot_path}`);
  lines.push(`Health JSON provided: ${r.meta.health_json_provided}`);
  lines.push("");
  lines.push("## 1. Schema Version");
  lines.push(`Expected: ${r.schema_version.expected}, actual: ${r.schema_version.actual} — **${r.schema_version.match ? "match" : "MISMATCH"}**`);
  lines.push("");
  lines.push("## 2. Hash-Chain Status");
  lines.push(`Integrity: **${r.hash_chain.integrity}** (${r.hash_chain.matching_entries}/${r.hash_chain.total_entries} entries match)`);
  if (r.hash_chain.first_failing_sequence !== null) {
    lines.push(`First failing sequence: ${r.hash_chain.first_failing_sequence}`);
  }
  lines.push("");
  lines.push("## 3. Recent Runs");
  lines.push(`Total: ${r.recent_runs.total}`);
  lines.push("By status: " + Object.entries(r.recent_runs.by_status).map(([k, v]) => `${k}=${v}`).join(", "));
  if (r.recent_runs.latest_10.length > 0) {
    lines.push("");
    lines.push("| run_id | status | created |");
    lines.push("|---|---|---|");
    for (const run of r.recent_runs.latest_10) {
      lines.push(`| ${run.id} | ${run.status} | ${run.created_at} |`);
    }
  }
  lines.push("");
  lines.push("## 4. Unknown Dispatches");
  lines.push(`Count: ${r.unknown_dispatches.count}`);
  if (r.unknown_dispatches.oldest_at) lines.push(`Oldest: ${r.unknown_dispatches.oldest_at}`);
  lines.push("");
  lines.push("## 5. Projection Drift (Outbox)");
  lines.push(`Count: ${r.projection_drift.count}`);
  if (r.projection_drift.latest_at) lines.push(`Latest: ${r.projection_drift.latest_at}`);
  lines.push("");
  lines.push("## 6. Undelivered Ingress");
  lines.push(`Count: ${r.undelivered_ingress.count}`);
  if (r.undelivered_ingress.oldest_at) lines.push(`Oldest: ${r.undelivered_ingress.oldest_at}`);
  lines.push("");
  lines.push("## 7. Approval Waits & Expiries");
  lines.push(`Waiting: ${r.approval.waiting_count}, expired: ${r.approval.expired_count}`);
  if (r.approval.oldest_pending_at) lines.push(`Oldest pending: ${r.approval.oldest_pending_at}`);
  lines.push("");
  lines.push("## 8. Duplicate-Reply Safety Notes");
  lines.push(`Ingress dedup rows: ${r.duplicate_reply_safety.ingress_dedup_count}`);
  if (r.duplicate_reply_safety.oldest_dedup_at) lines.push(`Oldest dedup: ${r.duplicate_reply_safety.oldest_dedup_at}`);
  lines.push(`Idempotency-key collisions: ${r.duplicate_reply_safety.idempotency_collisions}`);
  lines.push("");
  return lines.join("\n");
}

// --- Main ---

function main() {
  const args = parseArgs(process.argv);
  const db = openReadOnly(args.db);
  requireTables(db, ["journal_events", "runs", "ingress_dedup", "outbox_dispatches"]);

  let healthProvided = false;
  let healthData: any = null;
  if (args.health) {
    try {
      healthData = JSON.parse(readFileSync(args.health, "utf8"));
      healthProvided = true;
    } catch (e) {
      console.error(`error: cannot parse --health JSON: ${(e as Error).message}`);
      process.exit(EXIT_BAD_HEALTH);
    }
  }

  let gitRev = args.gitRev ?? null;
  if (!gitRev) {
    try {
      gitRev = execSync("git rev-parse HEAD", { encoding: "utf8" }).trim();
    } catch {
      gitRev = null;
    }
  }

  const report = {
    meta: {
      generated_at: new Date().toISOString(),
      git_revision: gitRev,
      db_snapshot_path: resolve(args.db),
      health_json_provided: healthProvided,
    },
    schema_version: schemaVersion(db),
    hash_chain: hashChain(db),
    recent_runs: recentRuns(db),
    unknown_dispatches: unknownDispatches(db),
    projection_drift: projectionDrift(db),
    undelivered_ingress: undeliveredIngress(db),
    approval: approval(db),
    duplicate_reply_safety: duplicateReplySafety(db),
    health: healthData,
  };

  db.close();
  writeReports(report, args.outDir);
  console.log(`audit report written to ${resolve(args.outDir)}/report.md and report.json`);
}

main();
