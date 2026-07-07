# Small Kernel, External Harness — Extension Hook & External Harness Boundary v0

> **Status**: Architecture decision record
> **Date**: 2026-07-07
> **Audience**: Agent Core Kernel and Harness developers

## Background

```
Dynamic Capability Runtime v0       — available
Feishu text approval v0             — available
CREATE capability smoke             — passed
UPGRADE capability smoke            — passed
Capability lifecycle                — dogfood-ready
```

- `DYNAMIC_CAPABILITY_LIFECYCLE_DOGFOOD_READY`
- `CREATE_APPROVE_EXECUTE_PASS`
- `UPGRADE_REAPPROVE_REEXECUTE_PASS`

With the capability lifecycle mainline proven stable, the focus shifts from the capability runtime core to:

- External Harness boundary definition
- Hook-based context extension
- Multi-entry routing
- Auto-growth through external harness
- Observability

## Purpose

This document freezes the boundary design between **Agent Core Kernel** and any **External Harness**
that builds product-layer functionality (Memory, Dream, Skill, Multi-Agent, Dashboard, Connector)
on top of the Kernel.

Its goal is to keep the Kernel minimal while providing explicit extension points — hooks, context,
events, decision policies, and resource references — through which an External Harness can implement
product-layer semantics without modifying the Kernel.

---

## Table of Contents

1. [Core Principles](#1-core-principles)
2. [Root Layout Contract](#2-root-layout-contract)
3. [Hook ABI Design](#3-hook-abi-design)
4. [ContextFragment Design](#4-contextfragment-design)
5. [ResourceRef / Skill Boundary](#5-resourceref--skill-boundary)
6. [Auto Approval Boundary](#6-auto-approval-boundary)
7. [Hook Hot Update Rules](#7-hook-hot-update-rules)
8. [Cross-platform Routing, Identity Binding, and Trust Boundaries](#8-cross-platform-routing-identity-binding-and-trust-boundaries)
9. [Dashboard / Observability](#9-dashboard--observability)
10. [External Harness Responsibilities](#10-external-harness-responsibilities)
11. [Non-Goals](#11-non-goals)
12. [Reference Design Influences](#12-reference-design-influences)
13. [Appendix: Product-layer Concepts That Should Be Externalized Through Hooks](#13-appendix-product-layer-concepts-that-should-be-externalized-through-hooks)

---

## 1. Core Principles

The following principles are inviolable contracts for the Kernel–Harness boundary:

1. **Kernel must not define product-layer concepts** such as Memory, Dream, Task, Harness workspace,
   or Dashboard state.
2. **Kernel may define a fixed root under `~/.agent-core`**, but only Kernel-owned subpaths are
   Kernel contract.
3. **Kernel owns** Run, Session, Event, Journal, Invocation, Decision, Capability, Snapshot, Receipt,
   and Hook.
4. **External Harness owns** memory files, dream reports, skill layout, task queues, dashboard state,
   multi-agent workspace, `AGENTS.md`-like files, and product-layer backups.
5. **Context is provided through hooks**, not hardcoded Kernel loaders.
6. **Skills must be supported through external resource / progressive-disclosure mechanisms**,
   not as hardcoded Kernel directory semantics.
7. **Executable capabilities remain Kernel-governed** through Gateway / Decision / Capability Host.
8. **Auto approval must go through policy hooks and formal Decision events**, not bypass Gateway.
9. **Dashboard must access Kernel state through stable read-only APIs**, not direct DB reads.
10. **Hook configuration may hot-update only after validation/health/smoke checks** and only at
    Run boundaries.
11. **Each Run must snapshot hook registry version** for reproducibility.

## 2. Root Layout Contract

```
~/.agent-core/
  kernel/
    state/
    journal/
    registry/
    receipts/
    snapshots/
    logs/
    backups/
    hook_registry.json

  workspace/
    default/
```

### Ownership

| Path | Owner | Contract |
|------|-------|----------|
| `kernel/` | Kernel | Kernel-owned. Kernel may read, write, and manage these paths freely. |
| `workspace/default/` | External Harness | Opaque to Kernel. Kernel may store or receive `workspace_id` / `workspace_path`, but must **not** inspect product-layer semantics inside the workspace. |

### Kernel Must Not Assume These Paths Exist

The External Harness may create any of the following, but none are part of the Kernel contract:

- `MEMORY.md`
- `AGENTS.md`
- `CLAUDE.md`
- `USER.md`
- `SOUL.md`
- `skills/`
- `dreams/`
- `tasks/`
- `subagents/`
- `harnesses/`
- `extensions/`
- `backups/`

Kernel must not crash, degrade, or alter behavior when any combination of these paths is absent.

## 3. Hook ABI Design

Six hook types are defined. The Kernel invokes these hooks at specific lifecycle points;
the External Harness registers implementations via `hook_registry.json`.

All hook calls produce a `HookInvocationReceipt` or equivalent Journal evidence.

### 3.1 `ingress.route.v0`

**Called after Connector validates the incoming platform event and before Session resolution.**

Maps a `ValidatedEvent` to `workspace_id`, `agent_id`, `session_key`, `context_profile`, and
`decision_policy_profile`. Routing is separate from context construction. The route hook decides
where the message belongs; the context hook builds dynamic context afterward.

**Example input:**

```json
{
  "hook": "ingress.route.v0",
  "event_id": "evt_xxx",
  "channel": {
    "kind": "feishu",
    "tenant_id": "tenant_x",
    "conversation_type": "group",
    "conversation_id": "chat_x",
    "thread_id": "optional_thread_x"
  },
  "principal": {
    "kind": "user",
    "id": "ou_xxx",
    "roles": ["member"]
  },
  "message": {
    "text_preview": "continue fixing the upgrade bug"
  }
}
```

**Example output:**

```json
{
  "workspace_id": "agent-core-dev",
  "agent_id": "architect",
  "session_key": "feishu:tenant_x:chat:chat_x",
  "session_policy": "conversation",
  "context_profile": "agent-core-dev",
  "decision_policy_profile": "dev-low-risk",
  "trust_boundary_id": "personal-dev",
  "principal_binding_id": "yanfen-primary",
  "route_policy_id": "route-table-v0",
  "route_policy_version": "2026-07-07",
  "reason": "matched Feishu group chat_x to agent-core-dev workspace"
}
```

**Security constraints:**

- Route hook can select workspace / agent / session, but **cannot elevate authority**.
- Route hook **cannot** mark a sender as owner.
- Route hook **cannot** grant decision token, shell, network, filesystem, or deployment permissions.
- Route hook **cannot** approve capability proposals.
- Route hook **cannot** override Gateway risk classification.

### 3.2 `context.prepare.v0`

**Called before Runtime builds dynamic context, after routing is resolved.**

| Aspect | Detail |
|--------|--------|
| Receives | `run_id`, `session_id`, `workspace_id`, `agent_id`, `session_key`, `channel`, `principal`, current user input, context budget |
| Returns | `system_append_fragments`, `user_context_fragments`, `resource_refs` |
| Constraints | Must not override immutable Kernel system prompt. All returned fragments must include `source`, `priority`, `estimated_token_cost`, `ttl`, `sensitivity`, and `hook_id`. |

**Regarding user input:**

> By default, `context.prepare` may receive the current user input because retrieval usually needs
> the query. Kernel must not send immutable system prompt, secrets, hidden chain-of-thought, or
> private tool state to context hooks. Connectors or policy may redact user input for sensitive
> channels.

### 3.3 `context.load.v0`

**Used for progressive disclosure.**

| Aspect | Detail |
|--------|--------|
| Flow | Kernel/Runtime initially receives only `ResourceRef` entries. When the model needs detail, it requests a specific `resource_id`. |
| Returns | The resource detail from the External Harness. |
| Kernel knowledge | Kernel does not know whether the resource is a skill, memory item, task, dream, note, or document. |

### 3.4 `context.compress.v0`

**Used for context compression/summarization.**

| Aspect | Detail |
|--------|--------|
| Role | Part of the context construction path, **not** the post-run learning path. |
| Receives | Budget, event/context range, compression purpose. |
| Returns | Compressed fragments with: source event ids, lossiness metadata, compressor id/version, token estimate. |
| Constraint | Kernel does **not** implement product-layer memory compression. |

### 3.5 `event.observe.v0`

**Used by External Harness for learning/reflection loops after events or runs are recorded.**

| Aspect | Detail |
|--------|--------|
| Timing | After events or runs are recorded. Not used to construct the **current** Run context. |
| Use | External Harness may update memory, dreams, skills, tasks, dashboards, or other product-layer state. |
| Mechanism | Prefer pull-based event cursor first, with optional push hook later. |
| Kernel knowledge | Kernel exposes events but does **not** know what External Harness learns from them. |

### 3.6 `decision.policy.v0`

**Called when a capability proposal is created or ready for decision.**

| Aspect | Detail |
|--------|--------|
| Returns | `auto_approve`, `deny`, `manual_required`, or `defer`. |
| Constraints | Auto approval must still produce a formal **Decision** event and go through normal digest validation and snapshot activation. Policy hook must **not** bypass `artifact_digest` / `manifest_digest` checks. |

## 4. ContextFragment Design

A `ContextFragment` is a structured piece of dynamic context injected into the model's context window.

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique fragment identifier |
| `hook_id` | string | Which hook produced this fragment |
| `kind` | enum | `instruction` \| `fact` \| `reference` \| `warning` \| `constraint` |
| `placement` | enum | `system_append` \| `user_context` |
| `priority` | integer | Higher priority fragments are included first within budget |
| `content` | string | The actual text content |
| `source` | string | Origin of the content (e.g., hook name, file path) |
| `ttl` | duration | Time-to-live; fragment expires after this duration |
| `estimated_tokens` | integer | Estimated token count for budget management |
| `sensitivity` | enum | `public` \| `internal` \| `sensitive` \| `secret` |

### Placement Rules

| Rule | Detail |
|------|--------|
| `system_append` | Placed below the immutable Kernel system prompt. Can **only** come from trusted allowlisted hooks. |
| `user_context` | Reference material, **not** policy. Lower trust level. |

### Restrictions

- Fragments are **dynamic context** and **cannot grant permissions**.
- Fragments **cannot modify** tool permissions, approval state, or Gateway decisions.
- Sensitivity affects filtering: `secret`-level fragments may be excluded for certain channels or
  audit levels.

## 5. ResourceRef / Skill Boundary

### ResourceRef

Kernel may understand `ResourceRef` — a lightweight reference with an `id`, `kind` hint,
and source metadata. The Kernel can store, pass, and resolve `ResourceRef`s through
`context.load.v0`.

### Skill

> Kernel must **not** understand Skill as a first-class Kernel primitive.

External Harness **may** implement Claude/OpenClaw/Hermes-style skills behind `ResourceRef`.
Skill-compatible support is required through progressive disclosure (`context.load.v0`).

### Execution Boundary

| If a skill only... | It is... | Mechanism |
|-------------------|----------|-----------|
| Provides instructions, examples, references, or workflow guidance | **Context** | Included via `context.prepare.v0` fragments |
| Runs code, changes state, accesses files, uses network, or mutates external systems | **Must use Capability/Gateway** | Executable capability governed by Kernel |

> **Skill scripts are not executable by default.** Only capabilities registered through
> the Gateway can execute code or mutate state.

## 6. Auto Approval Boundary

### Rules

- **Auto approval is policy-driven Decision, not Gateway bypass.**
- **Default mode is manual/off.**
- Development mode **may** allow low-risk `CREATE` auto approval through `decision.policy.v0`.
- `UPGRADE` auto approval should remain **disabled** until UPGRADE smoke is proven stable.

### Never Auto Approve

The following proposals must **never** be auto-approved:

- Kernel code mutation
- DB migration
- Unrestricted shell / network
- Secret / env changes
- Deploy / restart
- Permission expansion
- Missing digest
- Proposal with failed tests
- Proposal with unresolved risk classification

## 7. Hook Hot Update Rules

Hook registry can hot-update only through a **staged activation process**.

### Activation Stages

A proposed hook registry must pass:

1. **Schema validation** — structure and types are correct.
2. **Endpoint health check** — all hook endpoints are reachable.
3. **Optional smoke request** — a lightweight end-to-end test of the hook chain.
4. **Policy validation** — the proposed registry is consistent with current policy.

### Runtime Behavior

| Rule | Detail |
|------|--------|
| Activation boundary | Only after passing validation can the new registry become active for the **next** Run. |
| Running Run isolation | Hook changes do **not** affect an already-running Run. |
| Failure mode | Hook failure behavior must be explicit: `fail_open`, `fail_closed`, or `degrade`. |
| Evidence | All hook calls produce `HookInvocationReceipt` or equivalent Journal evidence. |
| Rollback | If validation fails, keep the previous active registry and record `hook_registry_rejected`. |
| Reproducibility | Each Run records the hook registry snapshot/version it was started with. |

## 8. Cross-platform Routing, Identity Binding, and Trust Boundaries

### Background

The design must support, over time:

- Different platform entry points (Feishu, Slack, Web, CLI)
- Multiple Feishu groups / Slack channels / Web sessions / CLI sessions
- Complex route configurations
- Multiple entry points mapped to the same workspace / agent / session
- External Harness reading `AGENTS.md`-like files from the resolved workspace
- Shared sessions across groups or platforms

### Core Constraints

```
1. Routing is not authorization.
2. Session key is not an authorization token.
3. Workspace is not a sandbox.
4. Route hook can select workspace / agent / session, but cannot elevate authority.
5. Cross-platform same-session routing must be explicit, not inferred.
6. Cross-platform same-session routing requires compatible trust_boundary_id.
```

### RouteDecision

`workspace_id`, `agent_id`, `session_key` — opaque routing metadata. `trust_boundary_id` — security/trust domain. `principal_binding_id` — optional cross-platform identity link. `session_policy` — lifecycle (`conversation`, `thread`, `ephemeral`, `user_scoped`). `context_profile`, `decision_policy_profile` — profile selectors. `route_policy_id`/`version` — external Harness route config. `reason` — human-readable explanation.

### IdentityBinding

`IdentityBinding` links platform-specific principals that are known to represent the same trusted actor or group. This is a concept for the External Harness, not a Kernel data structure in v0.

**Conceptual example:**

```json
{
  "principal_binding_id": "yanfen-primary",
  "trust_boundary_id": "personal-dev",
  "members": [
    { "platform": "feishu", "principal_id": "ou_xxx" },
    { "platform": "web", "principal_id": "local_owner" },
    { "platform": "cli", "principal_id": "os_user:yanfenma" }
  ]
}
```

**Rules:** Kernel does **not** resolve identity binding itself in v0. External Harness **may** return `principal_binding_id` in the route decision; Kernel records it as evidence but must **not** treat it alone as proof of authorization. Authorization remains based on validated principal, channel trust, Gateway policy, and capability permissions.

### Cross-platform Same-Session Rules

Default: one platform conversation maps to one session. Shared session across platforms must be explicitly configured by external Harness, requires same or compatible `trust_boundary_id`, and records all contributing channel/principal identities. Session key is routing metadata only, not authorization.

### Route Hook Fallback Rules

If `ingress.route.v0` fails:

- **Owner private chat** may optionally fallback to default workspace/session.
- **Group chats** should default to `fail_closed` or no-op response.
- **High-risk or untrusted channels** must `fail_closed`.
- Fallback behavior must be **explicit in hook registry**.
- Fallback events must be **written to Journal**.

### RouteDecision Journal Evidence

Each route decision must record: `event_id`, `validated principal`, `channel metadata`, `route hook id/version`, `route policy id/version`, `request hash`, `response hash`, `workspace_id`, `agent_id`, `session_key`, `trust_boundary_id`, `principal_binding_id`, `fallback status`, `latency`, `error if any`.

## 9. Dashboard / Observability

### Kernel API Surface

Dashboard must use **stable read-only Kernel APIs**:

- Events
- Runs
- Proposals
- Decisions
- Snapshots
- Capabilities
- Receipts

### Rules

- Dashboard must **not** read Kernel DB directly.
- Memory / Dream / Skill / Task dashboard data should come from **External Harness APIs**, not Kernel APIs.
- Kernel APIs expose **Kernel facts**, not product-layer state.

## 10. External Harness Responsibilities

The External Harness may implement, own, and evolve:

- Workspace layout
- `AGENTS.md` / `USER.md` / `SOUL.md` files
- Memory files and indexes
- Dream reports
- Skill directories and progressive disclosure
- Task queues
- Multi-agent workspace
- Dashboard state
- Product-layer backups
- Connector-specific presentation
- Connector-specific routing tables
- Decision policy logic

> Agent Core **intentionally does not copy** OpenClaw, Claude Code, or Hermes directory layouts into Kernel. External Harness can emulate or adapt those layouts behind hook/resource APIs.

## 11. Non-Goals

This document explicitly does not:

- Implement hooks
- Add Memory/Dream/Task modules to Kernel
- Define Harness internal workspace layout
- Implement Dashboard
- Implement auto approval
- Change Feishu Connector
- Change Capability Host or Coding Harness
- Change database schema

All of the above remain future work or External Harness concerns.

## 12. Reference Design Influences

- **OpenClaw** — workspace memory and dreaming inspired external, file-based, user-visible product-layer state and cross-channel routing.
- **Claude Code** — memory/skills/hooks inspired progressive disclosure and lifecycle hook extension.
- **Hermes** — learning loop inspired external Harness-owned memory/skill improvement.
- Agent Core intentionally adopts the pattern but **not** their directory layout as Kernel contract.

---
## 13. Appendix: Product-layer Concepts That Should Be Externalized Through Hooks

| Concept                       | Owner                                     | Externalization                                 |
| ----------------------------- | ----------------------------------------- | ----------------------------------------------- |
| Feishu group routing          | External Harness                          | `ingress.route.v0`                              |
| AGENTS.md / USER.md / SOUL.md | External Harness                          | `context.prepare.v0`                            |
| Memory                        | External Harness                          | `context.prepare.v0` + `event.observe.v0`       |
| Dream                         | External Harness                          | `event.observe.v0` + scheduled external Harness |
| Skill                         | External Harness                          | `ResourceRef` + `context.load.v0`               |
| Dashboard                     | External Harness                          | Kernel read-only API + Harness API              |
| Auto approval                 | Kernel Decision + External Harness policy | `decision.policy.v0`                            |
| Compression / summarization   | External Harness                          | `context.compress.v0`                           |
| Multi-agent task queue        | External Harness                          | route / context / event hooks                   |
| Capability execution          | Kernel governed                           | Gateway / Decision / Capability Host            |

---

*End of document.*
