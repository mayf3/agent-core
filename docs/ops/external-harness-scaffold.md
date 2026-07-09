# External Harness Scaffold v0

> **Audience**: Agent Core operators, developers, and agents
> **Prerequisites**:
> - Node.js (for running the generated Harness)
> - Coding Harness workspace configured (see [harness-workspace-bootstrap](./harness-workspace-bootstrap.md))

## Overview

The **External Harness Scaffold v0** is a script (`scaffold-context-harness.sh`)
that generates a standard external Harness project in
`~/.agent-core/harnesses/<harness_id>/`.

The generated Harness implements the `context.prepare.v0` hook and is ready
for local development, testing, and smoke verification.

### What it creates

```
~/.agent-core/harnesses/<harness_id>/
  README.md                 # Usage and registration instructions
  package.json              # Node project (ESM, no external dependencies)
  server.mjs                # Hook server (GET /health, POST /context.prepare.v0)
  test/server.test.mjs      # Test suite (5 tests, Node built-in runner)
  harness.manifest.json     # Harness manifest v0
```

### What it does NOT do

The scaffold script does **not**:

- Register the Harness with the Kernel
- Enable any Kernel hook
- Modify `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL`
- Modify shell profile or env
- Start the Harness server
- Modify the Feishu Connector
- Deploy or restart any service
- Expose coding tools to non-owner users

## Usage

### Basic usage

```bash
tools/coding-harness/scripts/scaffold-context-harness.sh my-first-harness
```

Creates `~/.agent-core/harnesses/my-first-harness/` with all scaffold files.

### Custom root directory

```bash
tools/coding-harness/scripts/scaffold-context-harness.sh \
  --root /path/to/workspace \
  my-first-harness
```

### Help

```bash
tools/coding-harness/scripts/scaffold-context-harness.sh --help
```

## Harness ID rules

| Rule | Example | Valid? |
|------|---------|--------|
| Lowercase letters and digits only | `myharness123` | ✅ |
| Hyphens between segments | `my-harness` | ✅ |
| Single character | `a` | ✅ |
| Uppercase letters | `MyHarness` | ❌ |
| Spaces | `my harness` | ❌ |
| Leading hyphen | `-bad` | ❌ |
| Trailing hyphen | `bad-` | ❌ |
| Slashes or dots | `a/b` | ❌ |
| Empty string | `` | ❌ |

## What's generated

### `server.mjs`

The generated server provides two endpoints:

**`GET /health`** — returns:

```json
{"status": "ok"}
```

**`POST /context.prepare.v0`** — returns a HookResponseEnvelope with
a fixed smoke fragment:

```json
{
  "request_id": "<echoed from request>",
  "hook": "context.prepare.v0",
  "timestamp": "<ISO timestamp>",
  "payload": {
    "fragments": [
      {
        "id": "frag_<timestamp>",
        "hook_id": "context.prepare.v0",
        "kind": "fact",
        "placement": "user_context",
        "priority": 1,
        "content": "EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya",
        "source": "127.0.0.1:17400",
        "ttl_secs": null,
        "estimated_tokens": 10,
        "sensitivity": "internal"
      }
    ],
    "resource_refs": []
  }
}
```

### `harness.manifest.json`

The manifest follows the v0 schema defined in
[docs/architecture/harness-manifest-v0.md](../architecture/harness-manifest-v0.md).

Key fields:

| Field | Value |
|-------|-------|
| `schema_version` | `harness-manifest-v0` |
| `harness_id` | `<your-harness-id>` |
| `kind` | `context.prepare.v0` |
| `entrypoint.command` | `node server.mjs` |
| `endpoint.url` | `http://127.0.0.1:17400/context.prepare.v0` |
| `permissions.read_paths` | `[]` (empty) |
| `permissions.network` | `["127.0.0.1"]` |

> Manifest existence does **not** equal Harness registration or enablement.
> It is a reference for humans and agents.

### `test/server.test.mjs`

The test suite uses Node's built-in test runner (`node:test`) with no
external dependencies. It covers:

1. `GET /health` returns 200 with status ok
2. `POST /context.prepare.v0` returns valid response envelope with smoke word
3. `POST /context.prepare.v0` echoes request_id correctly
4. Unknown routes return 404
5. Invalid JSON body returns 400

## Verification steps

### 1. Run tests

```bash
cd ~/.agent-core/harnesses/<harness_id>
npm test
```

Expected output:

```
✔ GET /health returns ok
✔ POST /context.prepare.v0 returns response envelope
✔ POST /context.prepare.v0 echoes request_id
✔ unknown route returns 404
✔ invalid JSON body returns 400
ℹ tests 5
ℹ pass 5
```

### 2. Start the server

```bash
cd ~/.agent-core/harnesses/<harness_id>
npm start
```

### 3. Health check

```bash
curl http://127.0.0.1:17400/health
```

Expected: `{"status":"ok"}`

### 4. Hook endpoint smoke

```bash
curl -s -X POST http://127.0.0.1:17400/context.prepare.v0 \
  -H 'Content-Type: application/json' \
  -d '{"hook":"context.prepare.v0","request_id":"smoke-001","payload":{}}' | jq .
```

Expected: response contains `"EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya"`

### 5. Check manifest

```bash
cat ~/.agent-core/harnesses/<harness_id>/harness.manifest.json | jq .
```

Verify fields match expectations.

## Manual registration (optional)

This Harness does **not** automatically connect to the Kernel.

To register it manually, follow the step-by-step guide in:
[docs/ops/harness-manual-registration-smoke.md](./harness-manual-registration-smoke.md)

The key environment variables to set:

```bash
export AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true
export AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0
export AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open
```

Then restart the Kernel.

## Rollback

To disable the Harness:

1. Unset or disable the hook env vars
2. Restart the Kernel
3. Stop the Harness server

For detailed rollback strategies, see the
[manual registration doc](./harness-manual-registration-smoke.md#rollback).

---

*End of document.*
