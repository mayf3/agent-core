# Harness Manifest v0

> **Status**: Architecture decision record
> **Date**: 2026-07-10
> **Audience**: Agent Core Kernel and Harness developers

## Background

This document defines the **v0 schema and lifecycle** for `harness.manifest.json`
— the file that describes an external Harness, its identity, endpoint,
permissions, smoke contract, and rollback strategy.

It builds on the conventions established in
[External Harness Workspace Lifecycle v0](./external-harness-workspace-v0.md):

- External Harness code lives in `~/.agent-core/harnesses/<harness-name>/`
- The Coding Harness can operate on this directory via workspace ID `harness-dev`
- Manifest is **not** automatically registered with the Kernel
- Registration is a manual, explicit step

### Relationship to Kernel `HarnessManifest`

The Kernel already defines a Rust struct `HarnessManifest` in
`src/harness/manifest.rs` for capability-based external harness operations.
That struct is used for `external.coding_*` operation manifests and is
registered through the capability proposal/approval pipeline.

The `harness.manifest.json` defined here is a **user-facing, hook-oriented**
manifest. It describes a hook server (e.g. `context.prepare.v0`) that the
Kernel calls at runtime. The two manifests serve different purposes and
have different schemas.

| Manifest | Purpose | Registration | Consumer |
|----------|---------|-------------|----------|
| Kernel `HarnessManifest` | Capability operations (`external.coding_*`) | Capability proposal pipeline | Kernel Gateway |
| `harness.manifest.json` (this doc) | Hook endpoints (`context.prepare.v0`, etc.) | Manual env var + restart | Kernel Hook ABI |

The hook-oriented manifest does **not** need to be parsed by the Kernel in v0.
It is a **human and agent reference** that documents what a Harness does,
how to register it, and how to roll back.

---

## Table of Contents

1. [Schema](#1-schema)
2. [Field Classification](#2-field-classification)
3. [Field Rules and Validation](#3-field-rules-and-validation)
4. [Endpoint Policy](#4-endpoint-policy)
5. [Permissions Model](#5-permissions-model)
6. [Smoke Contract](#6-smoke-contract)
7. [Rollback Strategy](#7-rollback-strategy)
8. [Agent Proposal Boundary](#8-agent-proposal-boundary)
9. [Manifest Lifecycle](#9-manifest-lifecycle)
10. [Non-Goals](#10-non-goals)

---

## 1. Schema

```json
{
  "schema_version": "harness-manifest-v0",
  "harness_id": "context-markdown-harness",
  "kind": "context.prepare.v0",
  "owner": "local-owner",
  "entrypoint": {
    "command": "node server.ts",
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
    "read_paths": [
      "~/.agent-core/workspace/default/SOUL.md",
      "~/.agent-core/workspace/default/USER.md",
      "~/.agent-core/workspace/default/project_facts.md"
    ],
    "network": [
      "127.0.0.1"
    ]
  },
  "smoke": {
    "command": "npm test",
    "manual_prompt": "请根据外部上下文回答 smoke word。",
    "expected_observation": "LLM response reflects injected context fragment"
  },
  "rollback": {
    "strategy": "restore_previous_hook_env_or_disable_hook",
    "previous_endpoint_env": "AGENT_CORE_CONTEXT_PREPARE_HOOK_URL"
  }
}
```

---

## 2. Field Classification

### v0 Required

| Field | Type | Description |
|-------|------|-------------|
| `schema_version` | string | Must be `"harness-manifest-v0"` |
| `harness_id` | string | Unique identifier within the Kernel instance. Must match directory name convention: `~/.agent-core/harnesses/<harness_id>/` |
| `kind` | string | Must be a known `HookKind` value: `context.prepare.v0`, `ingress.route.v0`, `context.load.v0`, `context.compress.v0`, `event.observe.v0`, `decision.policy.v0` |
| `entrypoint` | object | How to start the Harness server. Contains `command` (string) and `cwd` (string, relative to harness root) |
| `endpoint` | object | Hook endpoint configuration. Contains `url` (string) and `local_only` (boolean) |

### v0 Optional

| Field | Type | Description |
|-------|------|-------------|
| `owner` | string | Human-readable identifier of who owns/maintains this Harness |
| `health` | object | Health check configuration. Contains `url` (string) and `expected_status` (integer). If absent, no health check is defined |
| `permissions` | object | Declared access requirements. See [Permissions Model](#5-permissions-model) |
| `smoke` | object | Smoke test contract. See [Smoke Contract](#6-smoke-contract) |
| `rollback` | object | Rollback strategy. See [Rollback Strategy](#7-rollback-strategy) |

### Future — Schema Reserved

These fields are reserved for future phases. Parsers should accept but not
enforce them in v0:

| Field | Future Purpose |
|-------|----------------|
| `description` | Human-readable description of what the Harness does |
| `version` | Semantic version of the Harness |
| `dependencies` | External dependencies required by the Harness |

### Future — Must Approve

These fields require explicit human approval before the Harness can be
connected to a production Kernel:

| Field | Reason |
|-------|--------|
| `permissions.read_paths` | Grants filesystem read access outside the harness directory |
| `permissions.network` | Grants network access to specified hosts/ports |
| `endpoint.url` | Must be loopback-only. Changing the endpoint requires re-validation |
| `kind` | Changing the hook kind changes which lifecycle point the Harness attaches to |
| `failure_mode` | How the Kernel behaves when the hook fails (for future extension) |

### Custom Fields

Unknown top-level keys should cause validation failure in any future
manifest parser. The schema is strict.

---

## 3. Field Rules and Validation

### `schema_version`

- Must be `"harness-manifest-v0"`.
- Future versions will use `"harness-manifest-v1"`, etc.

### `harness_id`

- Must be non-empty.
- Should match the directory name: `~/.agent-core/harnesses/<harness_id>/`.
- Must be unique within a Kernel instance. Duplicate IDs are rejected on
  registration (for future manifest registry).

### `kind`

- Must be a known `HookKind` value.
- v0 known kinds: `context.prepare.v0`, `ingress.route.v0`, `context.load.v0`,
  `context.compress.v0`, `event.observe.v0`, `decision.policy.v0`.
- Unknown kinds are rejected.

### `entrypoint`

```json
{
  "command": "node server.ts",
  "cwd": "."
}
```

- `command`: Shell command to start the Harness server. Should be a single
  command string, not a pipeline.
- `cwd`: Working directory relative to the harness root. `"."` means the
  harness root directory.

### `health`

```json
{
  "url": "http://127.0.0.1:17400/health",
  "expected_status": 200
}
```

- `url`: Health check endpoint. Must be loopback with explicit port.
- `expected_status`: Expected HTTP status code. Defaults to `200` if absent.

### `endpoint`

```json
{
  "url": "http://127.0.0.1:17400/context.prepare.v0",
  "local_only": true
}
```

- `url`: The full URL the Kernel will call to invoke this hook. Must be
  loopback (`127.0.0.1`, `localhost`, `[::1]`) with explicit port. No query
  string, no fragment, no userinfo.
- `local_only`: Must be `true` in v0. Remote endpoints are not supported.

---

## 4. Endpoint Policy

### Rules

| Constraint | Rule |
|------------|------|
| Loopback only | `127.0.0.1`, `localhost`, `[::1]` |
| Explicit port | Required. No implicit port resolution |
| Scheme | `http://` only |
| Query string | Forbidden |
| Fragment | Forbidden |
| Userinfo | Forbidden |
| Path | Must be absolute. Must not contain query or fragment separators |

### Rationale

Hook endpoints are Kernel-internal extension points. They execute in the
same trust domain as the Kernel. Allowing non-loopback endpoints would
introduce remote code execution risk. The local-only constraint ensures
that hook calls cannot be exfiltrated to external servers.

---

## 5. Permissions Model

### `permissions.read_paths`

Declares which filesystem paths the Harness reads at runtime. This is a
**declarative** field — it documents what the Harness accesses but does
not automatically grant access.

Example:

```json
"permissions": {
  "read_paths": [
    "~/.agent-core/workspace/default/SOUL.md",
    "~/.agent-core/workspace/default/USER.md",
    "~/.agent-core/workspace/default/project_facts.md"
  ]
}
```

Rules:
- All paths should be absolute or use `~` for home directory expansion.
- Paths outside `~/.agent-core/` require explicit human approval.
- In v0, this field is informational only — no Kernel enforcement.
- In a future phase, this field will be used by the Kernel to sandbox
  hook file access.

### `permissions.network`

Declares which network addresses the Harness connects to at runtime.

Example:

```json
"permissions": {
  "network": [
    "127.0.0.1"
  ]
}
```

Rules:
- In v0, only localhost addresses are allowed.
- External network access requires explicit human approval.
- In v0, this field is informational only — no Kernel enforcement.

### Approval sensitivity

| Field | Default | Approval Required For |
|-------|---------|----------------------|
| `read_paths` | `[]` (no paths) | Any path outside `~/.agent-core/harnesses/<id>/` |
| `network` | `["127.0.0.1"]` | Any non-localhost address |

---

## 6. Smoke Contract

The smoke section defines how to verify that a Harness is working correctly:

```json
"smoke": {
  "command": "npm test",
  "manual_prompt": "请根据外部上下文回答 smoke word。",
  "expected_observation": "LLM response reflects injected context fragment"
}
```

| Field | Description |
|-------|-------------|
| `command` | Shell command to run the Harness test suite. Exit code 0 indicates test pass. |
| `manual_prompt` | A prompt to send to the LLM (via the Kernel) to verify the hook is injecting context. |
| `expected_observation` | What the operator should look for in the LLM's response to confirm the hook is working. |

### Smoke verification flow

```
1. Run `npm test` (or equivalent) → harness tests pass
2. Start the Harness server → `curl /health` → 200 OK
3. Send a test request to the hook endpoint → valid response
4. Configure Kernel to use the hook → restart Kernel
5. Send the manual_prompt to the Kernel → observe LLM response
6. Check that the response contains the expected_observation
7. Check Journal for HookCallRecorded event with status=success
```

---

## 7. Rollback Strategy

```json
"rollback": {
  "strategy": "restore_previous_hook_env_or_disable_hook",
  "previous_endpoint_env": "AGENT_CORE_CONTEXT_PREPARE_HOOK_URL"
}
```

| Field | Description |
|-------|-------------|
| `strategy` | The rollback approach. v0 supports one strategy: restore previous env or disable hook. |
| `previous_endpoint_env` | The environment variable that holds the previous hook URL, so it can be restored. |

### Rollback triggers (v0 manual)

| Trigger | Action |
|---------|--------|
| Health check fails at registration time | Do not register. Fix the Harness first. |
| `context.prepare` returns error at smoke time | Do not enable. Fix the Harness first. |
| Kernel `RunFailed` after hook enablement | Restore previous `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL` env var, restart Kernel |
| `HookCallRecorded` status `failed`/`degraded` increases | Restore previous hook env or disable hook |
| LLM does not consume expected context | Rollback or keep in shadow mode (disabled) |

### Core principle

```
Rollback should prioritize disabling the hook,
not blindly switching to an old endpoint.
```

If the previous endpoint was also failing, restoring it just re-introduces
the failure. A disabled hook can be re-enabled after root cause analysis.

### v0 scope

All rollback actions in v0 are **manual**. There is no:
- Automatic health check polling
- Automatic hook disable on failure
- Automatic rollback to previous endpoint

The operator follows the runbook to diagnose and act.

---

## 8. Agent Proposal Boundary

### Actions the agent may propose autonomously

| Action | Description |
|--------|-------------|
| Generate `harness.manifest.json` | Create a complete manifest for a new Harness |
| Run Harness tests | Execute the smoke command |
| Run local health checks | `curl` the health and endpoint URLs |
| Propose manifest changes | Suggest modifications to any field |
| Open PR for manifest changes | Submit manifest changes for review |

### Actions that require human approval

| Action | Reason |
|--------|--------|
| Expand `permissions.read_paths` | Grants filesystem access outside the harness directory |
| Expand `permissions.network` | Grants network access to non-localhost addresses |
| Change `endpoint.url` | Changes what address the Kernel calls |
| Change `kind` | Changes which lifecycle point the Harness attaches to |
| Register hook with production Kernel | Modifies running Kernel configuration |
| Enable hook in production | Activates the hook for live user interactions |
| Modify rollback strategy | Changes how failures are handled |

### Principle

```
Agents can propose, humans approve.
```

The agent may generate, test, and propose manifest content freely. But any
change that affects Kernel security, network boundary, or production
behavior must go through human review before activation.

---

## 9. Manifest Lifecycle

```
Harness code in ~/.agent-core/harnesses/<id>/
  │
  ├── 1. Create harness.manifest.json
  │      (agent or human writes the manifest)
  │
  ├── 2. Validate manifest
  │      (check schema, field rules, endpoint policy)
  │
  ├── 3. Run tests
  │      (npm test or equivalent)
  │
  ├── 4. Start Harness server
  │      (node server.ts, etc.)
  │
  ├── 5. Health check
  │      (curl /health → 200)
  │
  ├── 6. Hook endpoint check
  │      (curl /context.prepare.v0 → valid response)
  │
  ├── 7. Manual registration
  │      (set AGENT_CORE_CONTEXT_PREPARE_HOOK_* env vars)
  │
  ├── 8. Restart Kernel
  │      (so new env vars take effect)
  │
  ├── 9. Smoke run
  │      (send test prompt, observe LLM response)
  │
  ├── 10. Verify Journal
  │       (check HookCallRecorded events)
  │
  ├── 11. Monitor
  │       (observe hook health and call receipts)
  │
  └── 12. Rollback if needed
          (restore previous env or disable hook)
```

---

## 10. Non-Goals

This document explicitly does not:

- Define Kernel Rust types for manifest parsing
- Implement automatic manifest registration
- Modify the Kernel Hook ABI
- Modify Feishu Connector
- Add HarnessProposal types
- Implement automatic rollback
- Implement manifest registry or persistence
- Change DB schema
- Add HTTP routes
- Deploy or restart any service

---

*End of document.*
