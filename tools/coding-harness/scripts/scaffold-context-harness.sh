#!/usr/bin/env bash
#
# scaffold-context-harness.sh — Create a scaffold context.prepare.v0 Harness
#
# Generates a standard external Harness project in:
#   <root>/<harness_id>/
#
# The generated Harness implements the context.prepare.v0 hook with
# a fixed smoke word. It includes a server, tests, manifest, and README.
#
# Usage:
#   tools/coding-harness/scripts/scaffold-context-harness.sh <harness_id>
#   tools/coding-harness/scripts/scaffold-context-harness.sh --root /path <harness_id>
#
# Arguments:
#   --root PATH   Root directory for harness workspaces (default: ~/.agent-core/harnesses)
#   <harness_id>  Identifier for the new harness (lowercase, digits, hyphens only)
#
# Exit codes:
#   0  — success
#   1  — invalid harness_id
#   2  — target already exists
#   3  — root not writable
#   4  — scaffold failed
#

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────

ROOT="${HOME}/.agent-core/harnesses"

# ── Parse arguments ───────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --root)
            if [[ -z "${2:-}" ]]; then
                echo "error: --root requires a path argument" >&2
                exit 3
            fi
            ROOT="$2"
            shift 2
            ;;
        --root=*)
            ROOT="${1#*=}"
            shift
            ;;
        --help|-h)
            echo "Usage: $(basename "$0") [--root PATH] <harness_id>"
            echo ""
            echo "Create a scaffold context.prepare.v0 Harness project."
            echo ""
            echo "  --root PATH   Root directory (default: ~/.agent-core/harnesses)"
            echo "  <harness_id>  Identifier for the new harness"
            exit 0
            ;;
        --*)
            echo "error: unknown option: $1" >&2
            exit 1
            ;;
        *)
            if [[ -n "${HARNESS_ID:-}" ]]; then
                echo "error: unexpected argument: $1" >&2
                exit 1
            fi
            HARNESS_ID="$1"
            shift
            ;;
    esac
done

# ── Validate harness_id ───────────────────────────────────────────────────

if [[ -z "${HARNESS_ID:-}" ]]; then
    echo "error: harness_id is required" >&2
    echo "usage: $(basename "$0") [--root PATH] <harness_id>" >&2
    exit 1
fi

id_invalid=
if ! echo "$HARNESS_ID" | grep -qE '^[a-z0-9-]+$'; then
    id_invalid=1
fi
if echo "$HARNESS_ID" | grep -qE '(^|-|--)'; then
    # Allow single hyphens between segments; reject leading/trailing/consecutive
    if echo "$HARNESS_ID" | grep -qE '^-|-$|--'; then
        id_invalid=1
    fi
fi
# Re-run clean check after hyphen edge cases
if ! echo "$HARNESS_ID" | grep -qE '^[a-z0-9][a-z0-9-]*[a-z0-9]$' && [[ ${#HARNESS_ID} -gt 1 ]]; then
    # Single char allowed: just [a-z0-9]
    if ! [[ "$HARNESS_ID" =~ ^[a-z0-9]$ ]]; then
        id_invalid=1
    fi
fi
# Override: let the simple pattern be the single source of truth
if ! echo "$HARNESS_ID" | grep -qE '^[a-z0-9]([a-z0-9-]*[a-z0-9])?$'; then
    echo "error: invalid harness_id '${HARNESS_ID}' — use only lowercase letters, digits, and hyphens" >&2
    echo "harness_id must not start or end with a hyphen" >&2
    exit 1
fi

# ── Resolve target path ───────────────────────────────────────────────────

TARGET="${ROOT}/${HARNESS_ID}"

# Check root is writable.
if [[ ! -d "$ROOT" ]]; then
    mkdir -p "$ROOT" 2>/dev/null || {
        echo "error: cannot create root directory: $ROOT" >&2
        exit 3
    }
fi
if [[ ! -w "$ROOT" ]]; then
    echo "error: root directory is not writable: $ROOT" >&2
    exit 3
fi

# Check if target exists and is non-empty.
if [[ -d "$TARGET" ]]; then
    if ls -A "$TARGET" 2>/dev/null | grep -q .; then
        echo "error: target directory already exists and is not empty: $TARGET" >&2
        echo "use a different harness_id or remove the directory manually" >&2
        exit 2
    fi
fi

# ── Create directory structure ────────────────────────────────────────────

mkdir -p "${TARGET}/test"

# ── Write harness.manifest.json ───────────────────────────────────────────

cat > "${TARGET}/harness.manifest.json" <<EOF
{
  "schema_version": "harness-manifest-v0",
  "harness_id": "${HARNESS_ID}",
  "kind": "context.prepare.v0",
  "owner": "local-owner",
  "entrypoint": {
    "command": "node server.mjs",
    "cwd": "."
  },
  "health": {
    "url": "http://127.0.0.1:17400/health",
    "expected_status": 200
  },
  "endpoint": {
    "url": "http://127.0.0.1:17400/context.prepare.v0",
    "local_only": true
  },
  "permissions": {
    "read_paths": [],
    "network": ["127.0.0.1"]
  },
  "smoke": {
    "command": "npm test",
    "manual_prompt": "请根据外部上下文回答 smoke word。",
    "expected_observation": "LLM response reflects EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya"
  },
  "rollback": {
    "strategy": "restore_previous_hook_env_or_disable_hook",
    "previous_endpoint_env": "AGENT_CORE_CONTEXT_PREPARE_HOOK_URL"
  }
}
EOF

# ── Write package.json ────────────────────────────────────────────────────

cat > "${TARGET}/package.json" <<'PKGJSON'
{
  "type": "module",
  "scripts": {
    "test": "node --test",
    "start": "node server.mjs"
  }
}
PKGJSON

# ── Write server.mjs ──────────────────────────────────────────────────────

cat > "${TARGET}/server.mjs" <<'SRVEOF'
import http from "node:http";

const PORT = parseInt(process.env.PORT || "17400", 10);
const HOST = process.env.HOST || "127.0.0.1";

function json(res, status, data) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(data) + "\n");
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf-8")));
    req.on("error", reject);
  });
}

export function createServer() {
  return http.createServer(async (req, res) => {
    const { method, url } = req;

    // GET /health
    if (method === "GET" && url === "/health") {
      json(res, 200, { status: "ok" });
      return;
    }

    // POST /context.prepare.v0
    if (method === "POST" && url === "/context.prepare.v0") {
      let body;
      try {
        body = await readBody(req);
      } catch {
        json(res, 400, { error: "cannot read request body" });
        return;
      }

      let parsed;
      try {
        parsed = JSON.parse(body);
      } catch {
        json(res, 400, { error: "invalid json" });
        return;
      }

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
              content: "EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya",
              source: `${HOST}:${PORT}`,
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

    // 404
    json(res, 404, { error: "not_found" });
  });
}

// Start when run directly.
if (process.argv[1] && (process.argv[1].endsWith("server.mjs") || process.argv[1].endsWith("/server.mjs"))) {
  const server = createServer();
  server.listen(PORT, HOST, () => {
    console.log(`scaffold harness listening on http://${HOST}:${PORT}`);
    console.log(`  GET  /health`);
    console.log(`  POST /context.prepare.v0`);
  });
}
SRVEOF

# ── Write test/server.test.mjs ────────────────────────────────────────────

cat > "${TARGET}/test/server.test.mjs" <<'TESTEOF'
import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { createServer } from "../server.mjs";

const PORT = 17401;
const BASE = `http://127.0.0.1:${PORT}`;

let server;

test.before(() => {
  return new Promise((resolve) => {
    server = createServer();
    server.listen(PORT, "127.0.0.1", () => resolve());
  });
});

test.after(() => {
  return new Promise((resolve) => {
    if (server) server.close(() => resolve());
    else resolve();
  });
});

function request(method, path, body) {
  return new Promise((resolve, reject) => {
    const opts = {
      hostname: "127.0.0.1",
      port: PORT,
      path,
      method,
      headers: body ? { "Content-Type": "application/json" } : undefined,
    };
    const req = http.request(opts, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => {
        const raw = Buffer.concat(chunks).toString("utf-8");
        let data;
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

test("GET /health returns ok", async () => {
  const { status, data } = await request("GET", "/health");
  assert.equal(status, 200);
  assert.equal(data.status, "ok");
});

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

  const { status, data } = await request("POST", "/context.prepare.v0", requestBody);

  assert.equal(status, 200);
  assert.equal(data.hook, "context.prepare.v0");
  assert.equal(data.request_id, "test-req-001");
  assert.ok(typeof data.timestamp === "string", "timestamp is a string");
  assert.ok(Array.isArray(data.payload.fragments), "fragments is an array");
  assert.equal(data.payload.fragments.length, 1);

  const frag = data.payload.fragments[0];
  assert.equal(frag.content, "EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya");
  assert.equal(frag.hook_id, "context.prepare.v0");
  assert.equal(frag.kind, "fact");
  assert.equal(frag.placement, "user_context");
  assert.equal(frag.sensitivity, "internal");

  assert.ok(Array.isArray(data.payload.resource_refs), "resource_refs is an array");
  assert.equal(data.payload.resource_refs.length, 0);
});

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

test("unknown route returns 404", async () => {
  const { status, data } = await request("GET", "/unknown");
  assert.equal(status, 404);
  assert.equal(data.error, "not_found");
});

test("invalid JSON body returns 400", async () => {
  const { status, data } = await request("POST", "/context.prepare.v0", "not json at all");
  assert.equal(status, 400);
  assert.equal(data.error, "invalid json");
});
TESTEOF

# ── Write README.md ───────────────────────────────────────────────────────

cat > "${TARGET}/README.md" <<README
# ${HARNESS_ID}

Scaffold context.prepare.v0 Harness generated by
[tools/coding-harness/scripts/scaffold-context-harness.sh](../../scripts/scaffold-context-harness.sh).

## What this is

This is an **external Harness** project that implements the
\`context.prepare.v0\` hook for Agent Core.

It is **not automatically registered** with the Kernel.
It is **not automatically enabled**.

## Quick start

### Install

\`\`\`bash
cd ${HARNESS_ID}
\`\`\`

### Run tests

\`\`\`bash
npm test
\`\`\`

### Start server

\`\`\`bash
npm start
\`\`\`

### Smoke test health

\`\`\`bash
curl http://127.0.0.1:17400/health
# → {"status":"ok"}
\`\`\`

### Smoke test context.prepare.v0

\`\`\`bash
curl -s -X POST http://127.0.0.1:17400/context.prepare.v0 \\
  -H 'Content-Type: application/json' \\
  -d '{"hook":"context.prepare.v0","request_id":"smoke-001","payload":{}}' | jq .
\`\`\`

Expected: response contains \`"EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya"\`

## Manual registration

This Harness does **not** automatically connect to the Kernel.

To register it manually:

1. Follow the steps in \`docs/ops/harness-manual-registration-smoke.md\`
2. Set \`AGENT_CORE_CONTEXT_PREPARE_HOOK_URL\` to \`http://127.0.0.1:17400/context.prepare.v0\`
3. Restart the Kernel

## Rollback

To disable this Harness:

1. Unset \`AGENT_CORE_CONTEXT_PREPARE_HOOK_URL\` or disable the hook
2. Restart the Kernel
3. Stop the server process

## Files

| File | Purpose |
|------|---------|
| \`harness.manifest.json\` | Harness manifest (v0 schema) |
| \`server.mjs\` | Hook server implementing \`context.prepare.v0\` |
| \`test/server.test.mjs\` | Test suite |
| \`package.json\` | Node project config |
| \`README.md\` | This file |
README

echo "✅ scaffold created: ${TARGET}"
echo ""
echo "  cd ${TARGET}"
echo "  npm test"
echo "  npm start"
echo ""
