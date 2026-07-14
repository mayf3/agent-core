import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, readFileSync, existsSync, statSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  createJsonlExecuteStore,
  createMemoryExecuteStore,
  type StoredExecuteRecord,
} from "./execute-store.js";

function rec(overrides: Partial<StoredExecuteRecord> = {}): StoredExecuteRecord {
  const now = new Date().toISOString();
  return {
    idempotencyKey: "key_1",
    invocationId: "inv_1",
    operation: "feishu.send_message",
    status: "sent",
    receiptSummary: { messageId: "om_reply_1" },
    createdAt: now,
    updatedAt: now,
    ...overrides,
  };
}

// --- persistence + load + dedup ---

test("JSONL store persists a record and loads it back", () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-store-"));
  const path = join(dir, "exec.jsonl");
  try {
    const store = createJsonlExecuteStore(path);
    store.set(rec());
    const fresh = createJsonlExecuteStore(path); // simulate restart
    const loaded = fresh.load();
    assert.equal(loaded.size, 1);
    assert.equal(loaded.get("key_1")?.status, "sent");
    assert.equal(loaded.get("key_1")?.receiptSummary?.messageId, "om_reply_1");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("after restart (new store object), a replayed key is found => dedup", () => {
  // The contract: connector restart should NOT re-send for a replayed key.
  const dir = mkdtempSync(join(tmpdir(), "exec-store-2"));
  const path = join(dir, "exec.jsonl");
  try {
    createJsonlExecuteStore(path).set(rec());
    const restarted = createJsonlExecuteStore(path);
    assert.ok(restarted.get("key_1"), "persisted key must be found after restart");
    assert.equal(restarted.get("key_1")?.status, "sent");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- failure not saved ---

test("store.set is only called by the server on success — a failed record with status 'failed' is NOT deduped as sent", () => {
  // This is the contract the execute-server honors: it calls store.set only
  // inside the .then (success) branch. Here we verify the store itself does
  // not treat a 'failed' record as a 'sent' dedup hit.
  const store = createMemoryExecuteStore();
  store.set(rec({ status: "failed", idempotencyKey: "key_fail" }));
  const got = store.get("key_fail");
  assert.equal(got?.status, "failed");
  // The server only short-circuits on status === "sent"; a failed record is
  // NOT a positive dedup, so the caller would retry.
  assert.notEqual(got?.status, "sent");
});

// --- compact ---

test("JSONL store compacts without losing live records", () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-store-compact"));
  const path = join(dir, "exec.jsonl");
  try {
    const store = createJsonlExecuteStore(path, { compactAfterBytes: 1 });
    // compactAfterBytes: 1 forces compaction after every append.
    store.set(rec({ idempotencyKey: "k1" }));
    store.set(rec({ idempotencyKey: "k2" }));
    store.set(rec({ idempotencyKey: "k3" }));
    const fresh = createJsonlExecuteStore(path);
    const loaded = fresh.load();
    assert.equal(loaded.size, 3, "all live records survive compaction");
    assert.ok(loaded.get("k1") && loaded.get("k2") && loaded.get("k3"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("compaction rewrites the file (size resets, no duplicate lines)", () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-store-compact-size"));
  const path = join(dir, "exec.jsonl");
  try {
    const store = createJsonlExecuteStore(path, { compactAfterBytes: 1 });
    store.set(rec({ idempotencyKey: "k1" }));
    store.set(rec({ idempotencyKey: "k1" })); // overwrite
    const text = readFileSync(path, "utf8").trim().split("\n");
    // After compaction, only the latest state of k1 should remain (1 line).
    assert.equal(text.length, 1, "compaction collapses overwrites");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- TTL / max-age ---

test("expired records are dropped on load (TTL)", () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-store-ttl"));
  const path = join(dir, "exec.jsonl");
  try {
    // Write a record with a very old updatedAt using a 1ms maxAge store.
    const writer = createJsonlExecuteStore(path, { maxAgeMs: 7 * 24 * 60 * 60 * 1000 });
    writer.set(rec({ updatedAt: "2000-01-01T00:00:00Z", idempotencyKey: "old" }));
    // Reload with a 1ms TTL: the old record should be dropped.
    const reader = createJsonlExecuteStore(path, { maxAgeMs: 1 });
    assert.equal(reader.load().size, 0, "expired record dropped");
    assert.equal(reader.get("old"), undefined);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- minimal fields / no secret leakage in persisted record ---

test("persisted record contains no Authorization/token/secret/full-response fields", () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-store-nosec"));
  const path = join(dir, "exec.jsonl");
  try {
    createJsonlExecuteStore(path).set(rec());
    const text = readFileSync(path, "utf8");
    const parsed = JSON.parse(text.trim());
    assert.equal(parsed.op, "set");
    const keys = Object.keys(parsed.state).sort();
    // Minimal field set only.
    assert.deepEqual(
      keys,
      ["createdAt", "idempotencyKey", "invocationId", "operation", "receiptSummary", "status", "updatedAt"],
    );
    assert.doesNotMatch(text, /authorization|token|secret|app_secret|tenant_access_token/i);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- memory store parity ---

test("memory store get/load mirror the JSONL contract", () => {
  const store = createMemoryExecuteStore([rec({ idempotencyKey: "m1" })]);
  assert.ok(store.get("m1"));
  assert.equal(store.load().size, 1);
  assert.equal(store.get("nope"), undefined);
});
