import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { createServer } from "../server.ts";

const PORT = 17401;
const BASE = `http://127.0.0.1:${PORT}`;

// ── Setup / teardown ────────────────────────────────────────────────────

let server: http.Server;

test.before(() => {
  return new Promise<void>((resolve) => {
    server = createServer();
    server.listen(PORT, "127.0.0.1", () => resolve());
  });
});

test.after(() => {
  return new Promise<void>((resolve) => {
    if (server) server.close(() => resolve());
    else resolve();
  });
});

// ── Helper ───────────────────────────────────────────────────────────────

function request(
  method: string,
  path: string,
  body?: string,
): Promise<{ status: number; data: any }> {
  return new Promise((resolve, reject) => {
    const opts: http.RequestOptions = {
      hostname: "127.0.0.1",
      port: PORT,
      path,
      method,
      headers: body ? { "Content-Type": "application/json" } : undefined,
    };
    const req = http.request(opts, (res) => {
      const chunks: Buffer[] = [];
      res.on("data", (chunk: Buffer) => chunks.push(chunk));
      res.on("end", () => {
        const raw = Buffer.concat(chunks).toString("utf-8");
        let data: any;
        try {
          data = JSON.parse(raw);
        } catch {
          data = raw;
        }
        resolve({ status: res.statusCode ?? 0, data });
      });
    });
    req.on("error", reject);
    if (body) req.write(body);
    req.end();
  });
}

// ── Test 1: GET /health ─────────────────────────────────────────────────

test("GET /health returns ok", async () => {
  const { status, data } = await request("GET", "/health");
  assert.equal(status, 200);
  assert.equal(data.status, "ok");
});

// ── Test 2: POST /context.prepare.v0 ────────────────────────────────────

test("POST /context.prepare.v0 returns response envelope", async () => {
  const requestBody = JSON.stringify({
    hook: "context.prepare.v0",
    request_id: "test-req-001",
    timestamp: new Date().toISOString(),
    payload: {
      run_id: "run-1",
      session_id: "sess-1",
    },
  });

  const { status, data } = await request(
    "POST",
    "/context.prepare.v0",
    requestBody,
  );

  assert.equal(status, 200);

  // Verify hook kind matches.
  assert.equal(data.hook, "context.prepare.v0");

  // Verify request_id is echoed back.
  assert.equal(data.request_id, "test-req-001");

  // Verify timestamp is present.
  assert.ok(typeof data.timestamp === "string", "timestamp is a string");

  // Verify payload structure.
  assert.ok(Array.isArray(data.payload.fragments), "fragments is an array");
  assert.equal(data.payload.fragments.length, 1);

  const frag = data.payload.fragments[0];
  assert.equal(frag.content, "EXTERNAL_CONTEXT_SMOKE_WORD: papaya");
  assert.equal(frag.hook_id, "context.prepare.v0");
  assert.equal(frag.kind, "fact");
  assert.equal(frag.placement, "user_context");
  assert.equal(frag.sensitivity, "internal");

  // Verify resource_refs is present and empty.
  assert.ok(
    Array.isArray(data.payload.resource_refs),
    "resource_refs is an array",
  );
  assert.equal(data.payload.resource_refs.length, 0);
});

// ── Test 3: request_id echoing ──────────────────────────────────────────

test("POST /context.prepare.v0 echoes request_id", async () => {
  const customId = "my-custom-id-987";
  const requestBody = JSON.stringify({
    hook: "context.prepare.v0",
    request_id: customId,
    timestamp: new Date().toISOString(),
    payload: {},
  });

  const { data } = await request("POST", "/context.prepare.v0", requestBody);
  assert.equal(data.request_id, customId);
});

// ── Test 4: 404 for unknown routes ──────────────────────────────────────

test("unknown route returns 404", async () => {
  const { status, data } = await request("GET", "/unknown");
  assert.equal(status, 404);
  assert.equal(data.error, "not_found");
});

// ── Test 5: Invalid JSON returns 400 ────────────────────────────────────

test("invalid JSON body returns 400", async () => {
  const { status, data } = await request(
    "POST",
    "/context.prepare.v0",
    "not json at all",
  );
  assert.equal(status, 400);
  assert.equal(data.error, "invalid json");
});
