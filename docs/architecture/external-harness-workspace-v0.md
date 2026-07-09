# External Harness Workspace Lifecycle v0

> **Status**: Architecture decision record
> **Date**: 2026-07-09
> **Audience**: Agent Core Kernel and Harness developers

## Background

Prior investigation has confirmed the following findings:

```
EXTERNAL_HARNESS_WORKSPACE_LIFECYCLE_INVESTIGATION_COMPLETE
HARNESS_AGENT_DEVELOPMENT_PATH_DEFINED
READY_FOR_DESIGN_OR_IMPLEMENTATION_PR
```

Key direction established:

- Business Harness code should not continue being placed in the Agent Core main repository.
- Agent Core main repository retains only the reference harness, protocol definitions, and smoke documentation.
- User-private or rapidly evolving Harnesses should live in an external workspace.
- Long-term goal: enable agents to develop, test, register, enable, roll back, and optimize Harnesses through conversation.

However, a full system is not yet warranted:

```
Coding Harness currently supports operating on external directories via
pre-configured workspaces;
but dynamic workspace addition, automatic Harness discovery, and manifest
creation are not yet supported.
Kernel currently has no HookRegistry persistence, HarnessProposal, or
hot-update hooks.
```

This document therefore defines the **Phase 0** design for the External Harness Workspace Lifecycle. It freezes the architecture boundary for the next 10–20 Harness-related PRs.

## Purpose

This document defines the **External Harness Workspace Lifecycle v0** — the directory convention, manifest schema, lifecycle stages, agent-driven workflow, and phase planning for introducing external Harness workspaces into Agent Core without modifying Kernel runtime code.

---

## Table of Contents

1. [Default External Harness Storage Location](#1-default-external-harness-storage-location)
2. [Main Repository versus External Harness Boundary](#2-main-repository-versus-external-harness-boundary)
3. [Agent-driven Harness Lifecycle](#3-agent-driven-harness-lifecycle)
4. [v0 and Future Phase Boundaries](#4-v0-and-future-phase-boundaries)
5. [`harness.manifest.json` v0 Draft](#5-harnessmanifestjson-v0-draft)
6. [Hook Endpoint Policy](#6-hook-endpoint-policy)
7. [Health / Smoke / Rollback](#7-health--smoke--rollback)
8. [Agent Self-Optimization Security Boundaries](#8-agent-self-optimization-security-boundaries)
9. [Feishu HarnessChangeRequest v0](#9-feishu-harnesschangerequest-v0)
10. [PR Phase Planning](#10-pr-phase-planning)

---

## 1. Default External Harness Storage Location

v0 recommends the following directory convention for external Harness workspaces:

```
~/.agent-core/harnesses/<harness-name>/
```

### Rationale

| Criterion | Recommendation | Reason |
|-----------|---------------|--------|
| Does not pollute Agent Core main repository | `~/.agent-core/harnesses/` | Harness code lives outside the agent-core repo tree entirely |
| Suitable for private/personal tools | `~/.agent-core/harnesses/` | No git filter or ACL needed for user-specific harnesses |
| Suitable for agent-driven development | `~/.agent-core/harnesses/` | Coding Harness can write to `~/.agent-core/` via pre-configured workspace entries |
| Easy to scan, register, and disable | `~/.agent-core/harnesses/` | Single parent directory; one `ls` yields all external harnesses |
| Can naturally evolve into independent git repos | `~/.agent-core/harnesses/<name>/` | Each subdirectory can be `git init`'d and pushed to a remote without restructuring |

### Relationship to `tools/`

| Path | Role |
|------|------|
| `tools/<harness>/` | Reference harness only — maintained in the main repo, versioned with Kernel |
| `~/.agent-core/harnesses/<name>/` | External user/agent Harness — not in the main repo, not versioned with Kernel |
| External GitHub (future) | Mature or shared Harnesses may live as standalone repositories |

### Directory layout within a harness workspace

A typical external Harness workspace will contain:

```
~/.agent-core/harnesses/context-markdown-harness/
  harness.manifest.json       # Manifest (v0 draft, see §5)
  server.ts                   # Entrypoint source
  package.json                # Dependencies
  tsconfig.json               # TypeScript config
  README.md                   # Usage documentation
  test/                       # Tests
    server.test.ts
  smoke/                      # Smoke test scripts (future)
    smoke.sh
```

This layout is a **convention, not a mandate**. The only required file is `harness.manifest.json`. All other files are Harness-dependent.

---

## 2. Main Repository versus External Harness Boundary

### Agent Core main repository owns

```
Kernel
Hook ABI
Capability ABI
Reference harness
Capability Host
Coding Harness
Feishu Connector
Architecture docs
Ops smoke docs
```

### External Harness workspace owns

```
User-private tools
Project memory reading
Local scripts
Business APIs
Rapid experimentation capabilities
Harnesses that can be created, modified, and tested by agents
```

### Principle

```
Kernel must not become a Harness Manager platform.
```

The Kernel is responsible for running Harness invocations through the
Hook ABI, **not** for creating, storing, listing, or managing Harness
workspace directories. All Harness lifecycle management (create, test,
register, update, rollback) belongs to the external workspace layer
and is driven by the Coding Harness or future agent tools.

### Scope of this document

This document **defines the external workspace convention** so that
future agent tools and the Coding Harness can operate on a known
directory structure. It does **not** add Harness management to the
Kernel.

---

## 3. Agent-driven Harness Lifecycle

### Target flow

```
Feishu / conversation expresses a need
  → Agent identifies a HarnessChangeRequest
  → Agent uses Coding Harness to create external Harness workspace
  → Agent writes code, manifest, tests, and README
  → Agent runs tests
  → Agent generates smoke results
  → Agent requests user approval for integration
  → User approves → Agent registers and enables the Harness
  → Agent runs smoke against the live Kernel
  → If smoke fails → Agent disables or rolls back
  → Subsequent agents can observe, diagnose, propose patches,
    test, open PRs, request approval, and deploy updates
```

### Stage details

| Stage | Actor | Action |
|-------|-------|--------|
| Need identification | LLM / Agent | Recognizes that a new or modified Harness is required |
| Workspace creation | Coding Harness | Creates `~/.agent-core/harnesses/<name>/` with scaffolding |
| Implementation | Agent (via Coding Harness) | Writes server code, config, tests, manifest |
| Validation | Agent (via Coding Harness) | Runs `npm test` or equivalent; checks manifest schema |
| Smoke generation | Agent | Runs a lightweight end-to-end exercise against the Harness |
| Approval | User | Reviews smoke results; approves or rejects |
| Registration | Agent / Manual | Adds hook endpoint env var or registry entry |
| Enablement | Agent / Manual | Flips enabled flag for the hook |
| Post-deploy smoke | Agent | Verifies Harness is healthy and producing expected output |
| Observation | Agent / Kernel | Monitors health checks, hook call receipts, error rates |
| Iteration | Agent | Diagnoses failures, proposes patches, runs tests, requests re-approval |

---

## 4. v0 and Future Phase Boundaries

### v0 explicitly does NOT include

```
v0 does NOT implement Kernel HarnessProposal
v0 does NOT auto-enable production hooks
v0 does NOT let Feishu Connector automatically modify launchd / env
v0 does NOT include a Dashboard
v0 does NOT include a Harness marketplace
v0 does NOT implement distributed harnesses
```

### v0 only defines

```
1. External directory convention (~/.agent-core/harnesses/<name>/)
2. harness.manifest.json v0 draft schema
3. Manual configuration / smoke checklist guidance
4. PR phase planning for subsequent phases
```

### Current constraints

| Constraint | Detail |
|------------|--------|
| Coding Harness workspaces | Currently pre-configured via environment variable `CODING_CONFIG`; no dynamic workspace creation yet |
| Hook registration | Manual via env vars (`AGENT_CORE_CONTEXT_PREPARE_HOOK_*`) |
| No HookRegistry persistence | `HookRegistryConfig` struct exists but is not wired into DB-backed persistence |
| No HarnessProposal type | No Rust type for proposing new harness registrations through the decision pipeline |
| No hot reload | Changing hook config requires Kernel restart |

All of these constraints are addressed in future phases (see §10).

---

## 5. `harness.manifest.json` v0 Draft

### Schema

```json
{
  "schema_version": "harness-manifest-v0",
  "harness_id": "context-markdown-harness",
  "kind": "context.prepare.v0",
  "entrypoint": "node server.ts",
  "endpoint_url": "http://127.0.0.1:17400/context.prepare.v0",
  "health_url": "http://127.0.0.1:17400/health"
}
```

### Field classification

| Classification | Fields | Notes |
|----------------|--------|-------|
| **v0 mandatory** | `schema_version`, `harness_id`, `kind`, `entrypoint`, `endpoint_url`, `health_url` | Must be present and valid for a manifest to be accepted |
| **v0 optional** | `description`, `author`, `version` | Informational; no validation enforcement in v0 |
| **Future — schema only** | `smoke.command`, `smoke.timeout_secs`, `rollback.command`, `dependencies` | Field names reserved; parsing may be lenient in v0 |
| **Future — must approve** | `permissions.read_paths`, `permissions.network` | Granting a Harness access to additional paths or network requires explicit user approval |

### Field rules

| Rule | Detail |
|------|--------|
| `endpoint_url` | Must be a loopback address (`127.0.0.1`, `localhost`, `[::1]`) with explicit port. No query string, no fragment, no userinfo. |
| `kind` | Must be a known `HookKind` value (`context.prepare.v0`, `ingress.route.v0`, etc.). Unknown kinds are rejected. |
| `harness_id` | Must be unique within the Kernel instance. Duplicate IDs are rejected on registration. |
| `permissions.*` | Not enforced in v0. Fields are reserved and must not be populated until the approval pipeline exists. |
| Custom fields | Not allowed in v0. The manifest schema is strict; unknown top-level keys cause validation failure. |

---

## 6. Hook Endpoint Policy

### v0 approach

v0 continues to use environment variables for manual hook registration:

```
AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true
AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0
AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open
AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS=5000
```

This is adequate for development and smoke testing. No changes to the env-based config are made in v0.

### MSS-1 milestones (v-next)

| Milestone | Description |
|-----------|-------------|
| MSS-1 | `HookRegistryConfig` persistence in DB (not just env) |
| MSS-1 | Endpoint loopback validation at registration time |
| MSS-1 | `SIGHUP` / reload support for config changes without full restart |
| MSS-2 | `HarnessProposal` / `Approval` flow through the Gateway |

### Endpoint constraints (all versions)

| Constraint | Rule |
|------------|------|
| Loopback only | `127.0.0.1`, `localhost`, `[::1]` |
| Explicit port | Required; no implicit port resolution |
| Query string | Forbidden |
| Fragment | Forbidden |
| Userinfo | Forbidden |
| Scheme | `http://` only (no `https://` for loopback in v0) |

---

## 7. Health / Smoke / Rollback

### Suggested flow

| Condition | Action |
|-----------|--------|
| Health check fails | Record event → disable hook |
| Hook call timeout | Fail open / degrade → after N consecutive failures, disable |
| `HookCallRecorded` error count increases | Auto-disable or alert operator |
| Schema parse failure on manifest | Reject registration or force rollback |
| LLM output does not contain expected word | Requires manual confirmation (not automatic rollback) |
| Manifest validation failure | Reject registration; keep previous active config |

### Core principle

```
Rollback should prioritize disabling the hook,
not blindly switching to an old endpoint.
```

Disabling a hook is a safer default than assuming the previous endpoint is still valid. If the old endpoint was also failing, switching back to it just re-introduces the failure. A disabled hook can be re-enabled manually after root cause analysis.

### v0 limitations

- No automatic rollback mechanism exists in v0.
- Health checking is manual (operator runs `curl` against `health_url`).
- Disabling a hook in v0 means unsetting the env var and restarting.

---

## 8. Agent Self-Optimization Security Boundaries

### Actions the agent may perform autonomously

| Action | Description |
|--------|-------------|
| Observe | Read hook call receipts, health status, error logs |
| Diagnose | Identify root cause of Harness failures |
| Propose patch | Generate code changes for the Harness |
| Run tests | Execute Harness test suite via Coding Harness |
| Open PR / commit | Submit code changes for review |
| Smoke | Run smoke tests against the Harness |
| Rollback on failed deployment | Disable the hook after a failed deployment |

### Actions that require human approval

| Action | Reason |
|--------|--------|
| Expand `permissions.read_paths` | Grants access to additional filesystem paths |
| Expand `permissions.network` | Grants network access to additional hosts/ports |
| Add new Harness kind | Changes which lifecycle point the Harness attaches to |
| Modify `endpoint_url` | Changes what address the Kernel calls |
| Modify `failure_mode` | Changes how the Kernel behaves when the hook fails |
| Connect to production runtime | Affects live user interactions |
| Modify Kernel schema / route | Changes Kernel-internal structures or API surface |
| Bypass Gateway decision | Skips authorization checks |
| Auto-enable disabled hook | Re-activates a previously failed hook |

---

## 9. Feishu HarnessChangeRequest v0

### Target interaction

```
User: "Create a markdown context harness that reads SOUL.md, USER.md,
       and project_facts.md."

Agent:
  1. Generates a plan for the Harness
  2. Uses Coding Harness to create ~/.agent-core/harnesses/context-markdown-harness/
  3. Writes server.ts, package.json, README, and harness.manifest.json
  4. Runs npm test
  5. Returns smoke results to the user
  6. Asks whether to integrate with the current Kernel
```

### v0 clarification

```
v0 does NOT require Feishu Connector to specially intercept
HarnessChangeRequest messages.

Intent recognition is performed by the LLM / Agent through normal
conversation understanding.
```

The Feishu Connector passes the user's message as-is. The Agent (running in the Kernel) identifies that the request is a HarnessChangeRequest based on the conversation context and its system prompt. No message routing or intent classification middleware is added in v0.

---

## 10. PR Phase Planning

### Phase 0: Design document only

| Aspect | Detail |
|--------|--------|
| **Goal** | Freeze architecture boundary for all subsequent phases |
| **Files changed** | `docs/architecture/external-harness-workspace-v0.md` (this document) |
| **Files NOT changed** | All Rust code, TypeScript code, Kernel runtime, Coding Harness, Feishu Connector, DB schema, routes, manifest parser |
| **Acceptance criteria** | Document reviewed and merged; all 10 sections present; manifest schema defined; phase plan published |
| **Deployment needed** | No |

### Phase 1: External workspace convention + manifest schema

| Aspect | Detail |
|--------|--------|
| **Goal** | Enforce the external directory convention; add `harness.manifest.json` parsing to a shared utility |
| **Files changed** | New `tools/harness-manifest/` utility or extend `tools/coding-harness`; add manifest JSON schema validation |
| **Files NOT changed** | Kernel Rust code, Feishu Connector |
| **Acceptance criteria** | Coding Harness can validate a `harness.manifest.json` file; test covers valid/invalid manifests |
| **Deployment needed** | No (tools only) |

### Phase 2: Coding Harness can create external Harness workspace

| Aspect | Detail |
|--------|--------|
| **Goal** | Agent can instruct Coding Harness to scaffold a new external Harness workspace |
| **Files changed** | `tools/coding-harness/src/` (new operation or extended workspace logic) |
| **Files NOT changed** | Kernel Rust code, Feishu Connector |
| **Acceptance criteria** | Coding Harness creates `~/.agent-core/harnesses/<name>/` with scaffolding; test verifies directory structure |
| **Deployment needed** | No |

### Phase 3: Manual registration + smoke

| Aspect | Detail |
|--------|--------|
| **Goal** | Provide a CLI or script for manual Harness registration; smoke checklist documented |
| **Files changed** | New registration script under `scripts/` or extended `tools/`; `docs/ops/` smoke doc |
| **Files NOT changed** | Kernel Rust runtime, Feishu Connector |
| **Acceptance criteria** | User can point a Harness manifest, validate it, and register it. Smoke doc exists and is verified. |
| **Deployment needed** | No |

### Phase 4: Feishu HarnessChangeRequest

| Aspect | Detail |
|--------|--------|
| **Goal** | Agent can recognize and execute a HarnessChangeRequest through normal conversation flow |
| **Files changed** | Agent system prompt / instructions; possibly `src/harness/` for lightweight change types; Coding Harness workspace extensions |
| **Files NOT changed** | Feishu Connector message routing; Kernel runtime hook wiring |
| **Acceptance criteria** | End-to-end test: user types request → Agent creates harness → tests pass → user approves → harness enabled |
| **Deployment needed** | No (still dev-mode) |

### Phase 5: Rollback / self-heal

| Aspect | Detail |
|--------|--------|
| **Goal** | Agent can detect Harness failure, disable hook, roll back, and notify user |
| **Files changed** | Agent monitoring scripts; `src/hook/` for hook call receipt analysis; `src/harness/control.rs` for rollback types |
| **Files NOT changed** | DB schema; Kernel Gateway; Feishu Connector |
| **Acceptance criteria** | Agent detects N consecutive hook failures → disables hook → records Journal event → notifies user |
| **Deployment needed** | No |

---

## Gates / Validation

Before merging any PR in this phase plan, run:

```bash
cargo fmt --check
cargo build --all-targets

cargo test --lib -p agent-core-kernel hook
cargo test --lib -p agent-core-kernel runtime
cargo test --lib -p agent-core-kernel
cargo test --test m1_schema_version

node scripts/check-local-secret-leaks.mjs
npm run check:harnesses
git diff --check origin/main...HEAD
node scripts/check-structure.mjs
git status --short --branch
```

If `check-structure.mjs` fails only due to pre-existing files
(`logs/feishu-connector.log`), report the failure but do **not**
delete or commit the file.

---

## Non-Goals

This document explicitly does not:

- Modify Rust code
- Modify TypeScript code
- Modify the Kernel Runtime
- Modify the Coding Harness
- Modify the Feishu Connector
- Modify the DB schema
- Add new HTTP routes
- Add a manifest parser
- Deploy or restart any service
- Implement Harness marketplace
- Implement distributed harness
- Implement Dashboard
- Implement auto-approval

---
