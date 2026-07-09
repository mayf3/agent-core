#!/usr/bin/env node

/**
 * Minimal External Context Harness v0 — a local HTTP server that implements
 * the `context.prepare.v0` hook endpoint for the Agent Core Kernel.
 *
 * This is NOT a Memory, Skill, Task, Dream, or Workspace system. It is a
 * minimal development/verification tool that returns a fixed context fragment.
 *
 * Security:
 * - Intended for local development only (bind to 127.0.0.1).
 * - No authentication or tokens in v0.
 * - Does not persist any user content.
 * - Does not log request bodies or full headers.
 *
 * Usage:
 *   node tools/context-harness/server.ts
 *   # or: npm run context-harness
 */

import http from "node:http";

const DEFAULT_PORT = parseInt(process.env.PORT || "17400", 10);
const DEFAULT_HOST = process.env.HOST || "127.0.0.1";

/** Respond with JSON and the given status code. */
function json(
  res: http.ServerResponse,
  status: number,
  data: unknown,
): void {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(data) + "\n");
}

/** Read the entire request body as a string. */
function readBody(req: http.IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk: Buffer) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf-8")));
    req.on("error", reject);
  });
}

/** Create the HTTP server without starting it. */
export function createServer(): http.Server {
  return http.createServer(async (req, res) => {
    const { method, url } = req;

    // ── GET /health ────────────────────────────────────────────────────────
    if (method === "GET" && url === "/health") {
      json(res, 200, { status: "ok" });
      return;
    }

    // ── POST /context.prepare.v0 ────────────────────────────────────────────
    if (method === "POST" && url === "/context.prepare.v0") {
      let body: string;
      try {
        body = await readBody(req);
      } catch {
        json(res, 400, { error: "cannot read request body" });
        return;
      }

      let parsed: Record<string, unknown>;
      try {
        parsed = JSON.parse(body);
      } catch {
        json(res, 400, { error: "invalid json" });
        return;
      }

      // Echo back the request_id from the envelope.
      const requestId =
        typeof parsed.request_id === "string"
          ? parsed.request_id
          : `ctx_${Date.now()}`;

      const response = {
        request_id: requestId,
        hook: "context.prepare.v0",
        timestamp: new Date().toISOString(),
        payload: {
          fragments: [
            {
              id: `frag_${Date.now()}`,
              hook_id: "context.prepare.v0",
              kind: "fact",
              placement: "user_context",
              priority: 1,
              content: "EXTERNAL_CONTEXT_SMOKE_WORD: papaya",
              source: "context-harness:v0",
              ttl_secs: null,
              estimated_tokens: 10,
              sensitivity: "internal",
            },
          ],
          resource_refs: [],
        },
      };

      json(res, 200, response);
      return;
    }

    // ── 404 for everything else ─────────────────────────────────────────────
    json(res, 404, { error: "not_found" });
  });
}

// When run directly (not imported as a module), start the server.
const requiredAsModule = !process.argv[1]?.endsWith("server.ts");
if (!requiredAsModule) {
  const server = createServer();
  server.listen(DEFAULT_PORT, DEFAULT_HOST, () => {
    console.log(`context-harness v0 listening on http://${DEFAULT_HOST}:${DEFAULT_PORT}`);
    console.log(`  GET  /health`);
    console.log(`  POST /context.prepare.v0`);
  });
}
