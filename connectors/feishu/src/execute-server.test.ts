import test from "node:test";
import assert from "node:assert/strict";
import { validateExecute } from "./execute-server.js";

/** A minimal valid execute payload. */
function validPayload(overrides: Record<string, unknown> = {}) {
  return {
    protocol_version: "v1",
    operation: "feishu.send_message",
    invocation_id: "inv_1",
    decision_id: "dec_1",
    idempotency_key: "idem_1",
    arguments: { message_id: "om_1", text: "hi" },
    ...overrides,
  };
}

test("validateExecute accepts a well-formed payload", () => {
  assert.doesNotThrow(() => validateExecute(validPayload()));
});

test("validateExecute rejects unsupported protocol version", () => {
  assert.throws(
    () => validateExecute(validPayload({ protocol_version: "v2" })),
    /unsupported protocol version/,
  );
});

test("validateExecute rejects an operation the connector cannot execute", () => {
  // The connector only knows feishu.send_message; defense in depth — the
  // kernel's catalog allowlist is the primary guard.
  assert.throws(
    () => validateExecute(validPayload({ operation: "shell.exec" })),
    /operation_not_allowed/,
  );
});

test("validateExecute rejects a payload missing required fields", () => {
  for (const field of ["invocation_id", "decision_id", "idempotency_key"]) {
    const payload = validPayload();
    delete payload[field];
    assert.throws(
      () => validateExecute(payload),
      /invalid execute payload/,
      `missing ${field} should reject`,
    );
  }
});

test("validateExecute rejects a payload missing message_id or text", () => {
  for (const arg of ["message_id", "text"]) {
    const payload = validPayload();
    delete (payload.arguments as Record<string, unknown>)[arg];
    assert.throws(
      () => validateExecute(payload),
      /invalid execute payload/,
      `missing arguments.${arg} should reject`,
    );
  }
});

// --- Phase 3: execute idempotency dedup contract (store-based, no HTTP server) ---
// The execute server's restart-dedup is driven by the ExecuteStore: if a key is
// present with status "sent", the server short-circuits. We assert that
// contract directly against the store (deterministic, no port races), plus a
// shortId (logging) contract test.

import { createMemoryExecuteStore, type StoredExecuteRecord } from "./execute-store.js";

test("execute dedup contract: a 'sent' record causes the server to short-circuit (no re-send)", () => {
  // Simulate the server's decision logic: it checks store.get(key)?.status === "sent".
  const store = createMemoryExecuteStore();
  store.set({
    idempotencyKey: "idem_1",
    invocationId: "inv_1",
    operation: "feishu.send_message",
    status: "sent",
    receiptSummary: { messageId: "om_reply_1" },
    createdAt: "2026-06-18T10:00:00Z",
    updatedAt: "2026-06-18T10:00:00Z",
  });
  const hit = store.get("idem_1");
  assert.ok(hit && hit.status === "sent", "a sent record is a positive dedup hit");
  // The server would return receiptSummary without calling sendReply.
  assert.equal(hit?.receiptSummary?.messageId, "om_reply_1");
});

test("execute dedup contract: a 'failed'/absent record does NOT short-circuit (retry allowed)", () => {
  const store = createMemoryExecuteStore();
  // No record => not deduped.
  assert.equal(store.get("absent"), undefined);
  // A failed record is not 'sent' => the server retries.
  store.set({
    idempotencyKey: "idem_fail",
    invocationId: "inv_1",
    operation: "feishu.send_message",
    status: "failed",
    createdAt: "2026-06-18T10:00:00Z",
    updatedAt: "2026-06-18T10:00:00Z",
  });
  const hit = store.get("idem_fail");
  assert.ok(!hit || hit.status !== "sent", "a failed record must not be a positive dedup");
});

import { createJsonlExecuteStore } from "./execute-store.js";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

test("execute dedup contract: persistence survives a new store object (restart)", () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-restart-"));
  try {
    const path = join(dir, "exec.jsonl");
    createJsonlExecuteStore(path).set({
      idempotencyKey: "idem_r",
      invocationId: "inv_r",
      operation: "feishu.send_message",
      status: "sent",
      receiptSummary: { messageId: "om_r" },
      createdAt: "2026-06-18T10:00:00Z",
      updatedAt: "2026-06-18T10:00:00Z",
    });
    const restarted = createJsonlExecuteStore(path);
    const hit = restarted.get("idem_r");
    assert.ok(hit && hit.status === "sent", "persisted 'sent' survives restart => dedup");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
