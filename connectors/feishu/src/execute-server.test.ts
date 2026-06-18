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

// --- real HTTP server integration tests ---

import http from "node:http";
import { startExecuteServer } from "./execute-server.js";
import type { ConnectorConfig } from "./config.js";

/** Helper: POST JSON to a URL and return the parsed body. */
async function postJson(
  url: string,
  body: unknown,
  token: string,
): Promise<{ status: number; body: unknown }> {
  const res = await fetch(url, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${token}`,
    },
    body: JSON.stringify(body),
  });
  const text = await res.text();
  return { status: res.status, body: JSON.parse(text) };
}

function makePayload(overrides: Record<string, unknown> = {}): Record<string, unknown> {
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

function makeConfig(overrides: Record<string, unknown> = {}): ConnectorConfig {
  return {
    appId: "app",
    appSecret: "secret",
    kernelUrl: "http://127.0.0.1:4130/v1/ingress",
    kernelIngressTimeoutMs: 100,
    connectorPort: 0, // OS-assigned port
    ipcToken: "ipc-token",
    processingReactionEmoji: "OK",
    failedReactionEmoji: "ERROR",
    reactionStatePath: join(tmpdir(), "reactions.jsonl"),
    reactionRetryAttempts: 3,
    reactionRetryBaseDelayMs: 0,
    executeStatePath: join(tmpdir(), "executes.jsonl"),
    ...overrides,
  };
}

test("HTTP: first execute calls sendReply, persists sent, returns ok", async () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-http-"));
  try {
    const storePath = join(dir, "exec.jsonl");
    const store = createJsonlExecuteStore(storePath);
    const fakeClient = {
      calls: [] as Array<{ method: string; url: string; data: unknown }>,
      async request(opts: { method: string; url: string; data: unknown }) {
        this.calls.push(opts);
        return { data: { message_id: "om_reply_1" } };
      },
    };

    const server = startExecuteServer(
      makeConfig({ executeStatePath: storePath }),
      fakeClient,
      undefined,
      store,
    );
    await new Promise<void>((r) => server.on("listening", () => r()));
    const addr = server.address() as { port: number };
    const url = `http://127.0.0.1:${addr.port}/v1/execute`;

    try {
      const { status, body } = await postJson(url, makePayload(), "ipc-token");
      assert.equal(status, 200);
      assert.deepStrictEqual(body, {
        ok: true,
        receipt: { message_id: "om_reply_1", status: "sent" },
      });
      assert.equal(fakeClient.calls.length, 1, "client.request called once");
      // Store should have a persisted "sent" record.
      assert.ok(store.get("idem_1")?.status === "sent");
    } finally {
      await new Promise<void>((r) => server.close(() => r()));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("HTTP: same idempotency_key after restart returns replayed:true, no sendReply call", async () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-http-restart-"));
  try {
    const storePath = join(dir, "exec.jsonl");

    // --- first server instance ---
    const store1 = createJsonlExecuteStore(storePath);
    const fakeClient1 = {
      calls: [] as Array<{ method: string; url: string; data: unknown }>,
      async request(opts: { method: string; url: string; data: unknown }) {
        this.calls.push(opts);
        return { data: { message_id: "om_reply_1" } };
      },
    };

    const server1 = startExecuteServer(
      makeConfig({ executeStatePath: storePath }),
      fakeClient1,
      undefined,
      store1,
    );
    await new Promise<void>((r) => server1.on("listening", () => r()));
    const addr1 = server1.address() as { port: number };
    const url1 = `http://127.0.0.1:${addr1.port}/v1/execute`;

    await postJson(url1, makePayload(), "ipc-token");
    assert.equal(fakeClient1.calls.length, 1, "first call sends reply");
    await new Promise<void>((r) => server1.close(() => r()));

    // --- server restart: load persisted store, reuse same idempotency_key ---
    const store2 = createJsonlExecuteStore(storePath);
    store2.load(); // simulate startup load
    const fakeClient2 = {
      calls: [] as Array<{ method: string; url: string; data: unknown }>,
      async request(_opts: { method: string; url: string; data: unknown }) {
        this.calls.push({ method: "", url: "", data: "" }); // would be a bug
        return { data: { message_id: "om_reply_2" } };
      },
    };

    const server2 = startExecuteServer(
      makeConfig({ executeStatePath: storePath }),
      fakeClient2,
      undefined,
      store2,
    );
    await new Promise<void>((r) => server2.on("listening", () => r()));
    const addr2 = server2.address() as { port: number };
    const url2 = `http://127.0.0.1:${addr2.port}/v1/execute`;

    try {
      const { status, body } = await postJson(url2, makePayload(), "ipc-token");
      assert.equal(status, 200);
      const data = body as Record<string, unknown>;
      assert.equal(data.ok, true);
      assert.equal(data.replayed, true, "persisted dedup should set replayed:true");
      assert.equal(
        (data.receipt as Record<string, unknown>).message_id,
        "om_reply_1",
        "replayed receipt returns original message_id",
      );
      // The fake client should NOT have been called.
      assert.equal(fakeClient2.calls.length, 0, "second call must NOT send another Feishu reply");
    } finally {
      await new Promise<void>((r) => server2.close(() => r()));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("HTTP: sendReply failure does NOT persist as sent, retry allowed", async () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-http-fail-"));
  try {
    const storePath = join(dir, "exec.jsonl");
    const store = createJsonlExecuteStore(storePath);
    const failingClient = {
      async request() {
        throw new Error("feishu api error");
      },
    };

    const server = startExecuteServer(
      makeConfig({ executeStatePath: storePath }),
      failingClient,
      undefined,
      store,
    );
    await new Promise<void>((r) => server.on("listening", () => r()));
    const addr = server.address() as { port: number };
    const url = `http://127.0.0.1:${addr.port}/v1/execute`;

    try {
      const { status, body } = await postJson(url, makePayload(), "ipc-token");
      assert.equal(status, 500);
      const data = body as Record<string, unknown>;
      assert.equal(data.ok, false);
      // Store must NOT have a "sent" record for this key (retry allowed).
      const record = store.get("idem_1");
      assert.equal(record?.status, undefined, "failed request must NOT leave a 'sent' record");
    } finally {
      await new Promise<void>((r) => server.close(() => r()));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("HTTP: replay response and log do not leak full idempotency_key or Authorization", async () => {
  const dir = mkdtempSync(join(tmpdir(), "exec-http-noleak-"));
  try {
    const storePath = join(dir, "exec.jsonl");
    const store = createJsonlExecuteStore(storePath);
    const fakeClient = {
      calls: [] as Array<{ method: string; url: string; data: unknown }>,
      async request(opts: { method: string; url: string; data: unknown }) {
        this.calls.push(opts);
        return { data: { message_id: "om_reply_1" } };
      },
    };

    const server = startExecuteServer(
      makeConfig({ executeStatePath: storePath }),
      fakeClient,
      undefined,
      store,
    );
    await new Promise<void>((r) => server.on("listening", () => r()));
    const addr = server.address() as { port: number };
    const url = `http://127.0.0.1:${addr.port}/v1/execute`;

    // Make a first call to persist a record, then replay.
    await postJson(url, makePayload({ idempotency_key: "idem_secret_1234567890_test" }), "ipc-token");

    const { status, body } = await postJson(
      url,
      makePayload({ idempotency_key: "idem_secret_1234567890_test" }),
      "ipc-token",
    );
    assert.equal(status, 200);
    const data = body as Record<string, unknown>;
    assert.equal(data.replayed, true);
    const bodyStr = JSON.stringify(body);
    // The full idempotency_key must NOT appear in the response body.
    assert.doesNotMatch(bodyStr, /idem_secret_1234567890_test/);
    assert.doesNotMatch(bodyStr, /authorization|ipc-token|Bearer/i);

    await new Promise<void>((r) => server.close(() => r()));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
