import test from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { mkdtempSync, rmSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";

const CLI = join(process.cwd(), "tools/audit-report/cli.ts");

/** Build a minimal valid Kernel-style snapshot with a verified hash chain. */
function buildFixture(path: string) {
  const db = new DatabaseSync(path);
  db.exec(`
    CREATE TABLE journal_events (
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
    CREATE TABLE runs (
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
    CREATE TABLE ingress_dedup (
      source TEXT NOT NULL,
      external_event_id TEXT NOT NULL,
      event_id TEXT NOT NULL,
      first_seen_at TEXT NOT NULL,
      PRIMARY KEY(source, external_event_id)
    );
    CREATE TABLE outbox_dispatches (
      dispatch_id TEXT PRIMARY KEY,
      invocation_id TEXT NOT NULL UNIQUE,
      run_id TEXT NOT NULL,
      session_id TEXT,
      operation TEXT NOT NULL,
      arguments_json TEXT NOT NULL,
      idempotency_key TEXT NOT NULL,
      decision_id TEXT NOT NULL DEFAULT '',
      acked_unknown INTEGER NOT NULL DEFAULT 0,
      status TEXT NOT NULL,
      attempts INTEGER NOT NULL DEFAULT 0,
      available_at TEXT NOT NULL,
      locked_by TEXT,
      locked_until TEXT,
      last_error TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );
    PRAGMA user_version = 1;
  `);

  function h(prev: string | null, seq: number, kind: string, payload: string) {
    const hasher = createHash("sha256");
    hasher.update((prev ?? "") + "|");
    hasher.update(String(seq) + "|");
    hasher.update(kind + "|");
    hasher.update(payload);
    return hasher.digest("hex");
  }

  let prev: string | null = null;
  // The IngressAccepted + its delivery chain (SessionReady/RunStarted/RunCompleted)
  // must share one correlation_id so the ingress is counted as delivered.
  const events: [string, string, string, string][] = [
    ["evt_1", "IngressAccepted", JSON.stringify({ event_id: "msg_1" }), "corr_1"],
    ["evt_2", "SessionReady", "{}", "corr_1"],
    ["evt_3", "RunStarted", "{}", "corr_1"],
    ["evt_4", "RunCompleted", "{}", "corr_1"],
  ];
  let seq = 0;
  for (const [eid, kind, payload, corr] of events) {
    seq++;
    const hash = h(prev, seq, kind, payload);
    db.prepare(
      `INSERT INTO journal_events (sequence, event_id, kind, payload_json, previous_hash, hash, correlation_id, created_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    ).run(seq, eid, kind, payload, prev, hash, corr, "2026-06-18T10:00:00Z");
    prev = hash;
  }

  db.prepare(
    `INSERT INTO runs (id, session_id, agent_id, trigger_event_id, principal_json, status, created_at, updated_at)
     VALUES ('run_1','sess_1','main','evt_1','{}','Completed','2026-06-18T10:00:00Z','2026-06-18T10:01:00Z')`,
  ).run();
  db.prepare(
    `INSERT INTO ingress_dedup (source, external_event_id, event_id, first_seen_at)
     VALUES ('feishu','msg_1','evt_1','2026-06-18T10:00:00Z')`,
  ).run();
  db.prepare(
    `INSERT INTO outbox_dispatches (dispatch_id, invocation_id, run_id, operation, arguments_json, idempotency_key, status, attempts, available_at, created_at, updated_at)
     VALUES ('d_1','inv_1','run_1','feishu.send_message','{}','idem_1','succeeded',1,'2026-06-18T10:00:00Z','2026-06-18T10:00:00Z','2026-06-18T10:00:30Z')`,
  ).run();
  db.close();
}

function runCli(dbPath: string, outDir: string, extra: string[] = []) {
  execFileSync("node", ["--experimental-strip-types", CLI, "--db", dbPath, "--out-dir", outDir, ...extra], {
    encoding: "utf8",
  });
}

test("audit report produces report.md + report.json with all 8 sections", () => {
  const dir = mkdtempSync(join(tmpdir(), "audit-"));
  const dbPath = join(dir, "snap.db");
  const outDir = join(dir, "out");
  buildFixture(dbPath);
  runCli(dbPath, outDir);

  assert.ok(existsSync(join(outDir, "report.md")));
  assert.ok(existsSync(join(outDir, "report.json")));
  const report = JSON.parse(readFileSync(join(outDir, "report.json"), "utf8"));

  // Schema version.
  assert.equal(report.schema_version.expected, 1);
  assert.equal(report.schema_version.actual, 1);
  assert.equal(report.schema_version.match, true);

  // Hash chain: all 4 entries match.
  assert.equal(report.hash_chain.total_entries, 4);
  assert.equal(report.hash_chain.matching_entries, 4);
  assert.equal(report.hash_chain.mismatched_entries, 0);
  assert.equal(report.hash_chain.integrity, "ok");

  // Recent runs.
  assert.equal(report.recent_runs.total, 1);
  assert.equal(report.recent_runs.by_status.Completed, 1);
  assert.equal(report.recent_runs.latest_10.length, 1);

  // Unknown dispatches: none.
  assert.equal(report.unknown_dispatches.count, 0);

  // Projection drift: none (no 'dispatching' rows).
  assert.equal(report.projection_drift.count, 0);

  // Undelivered ingress: the single IngressAccepted is delivered (SessionReady shares its corr).
  assert.equal(report.undelivered_ingress.count, 0);

  // Approval: none.
  assert.equal(report.approval.waiting_count, 0);
  assert.equal(report.approval.expired_count, 0);

  // Duplicate-reply safety.
  assert.equal(report.duplicate_reply_safety.ingress_dedup_count, 1);
  assert.equal(report.duplicate_reply_safety.idempotency_collisions, 0);

  rmSync(dir, { recursive: true, force: true });
});

test("audit report refuses to run without --db", () => {
  assert.throws(
    () => execFileSync("node", ["--experimental-strip-types", CLI, "--out-dir", "/tmp"], { encoding: "utf8" }),
    /--db is required/,
  );
});

test("audit report detects a corrupted hash chain", () => {
  const dir = mkdtempSync(join(tmpdir(), "audit-corrupt-"));
  const dbPath = join(dir, "snap.db");
  const outDir = join(dir, "out");
  buildFixture(dbPath);
  // Tamper with one event's payload (breaks its hash + the chain link).
  const db = new DatabaseSync(dbPath);
  db.prepare("UPDATE journal_events SET payload_json='{\"tampered\":true}' WHERE sequence=3").run();
  db.close();

  runCli(dbPath, outDir);
  const report = JSON.parse(readFileSync(join(outDir, "report.json"), "utf8"));
  assert.equal(report.hash_chain.integrity, "faulty");
  assert.equal(report.hash_chain.mismatched_entries > 0, true);
  assert.equal(report.hash_chain.first_failing_sequence, 3);

  rmSync(dir, { recursive: true, force: true });
});

test("audit report flags undelivered ingress", () => {
  const dir = mkdtempSync(join(tmpdir(), "audit-undelivered-"));
  const dbPath = join(dir, "snap.db");
  const outDir = join(dir, "out");
  buildFixture(dbPath);
  // Add an IngressAccepted with NO matching SessionReady/RunStarted/etc.
  const db = new DatabaseSync(dbPath);
  const payload = JSON.stringify({ event_id: "msg_orphan" });
  // Recompute the chain tail properly so we don't false-positive on integrity.
  const last = db.prepare("SELECT hash FROM journal_events ORDER BY sequence DESC LIMIT 1").get() as { hash: string };
  const seq = 5;
  const hasher = createHash("sha256");
  hasher.update(last.hash + "|");
  hasher.update(String(seq) + "|");
  hasher.update("IngressAccepted|");
  hasher.update(payload);
  const hash = hasher.digest("hex");
  db.prepare(
    `INSERT INTO journal_events (sequence, event_id, kind, payload_json, previous_hash, hash, correlation_id, created_at)
     VALUES (?, 'evt_orphan', 'IngressAccepted', ?, ?, ?, 'corr_orphan', '2026-06-18T11:00:00Z')`,
  ).run(seq, payload, last.hash, hash);
  db.close();

  runCli(dbPath, outDir);
  const report = JSON.parse(readFileSync(join(outDir, "report.json"), "utf8"));
  assert.equal(report.hash_chain.integrity, "ok", "orphan event must not corrupt the chain");
  assert.equal(report.undelivered_ingress.count, 1);

  rmSync(dir, { recursive: true, force: true });
});
