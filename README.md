# Agent Core

Agent Core is a small local-first agent kernel. Its job is not to become a full
agent platform. Its job is to provide stable primitives so agents can build,
load, and improve external capabilities around it.

## Direction

- Runtime: Rust kernel plus a small TypeScript Feishu connector.
- Kernel style: small core with explicit, non-bypassable boundaries.
- First usable channel: Feishu, so mobile messages can start and steer local runs.
- First model interface: OpenAI-compatible provider.
- First tool surface: reply to the current session only. Shell, filesystem tools,
  multi-agent orchestration, workflow graphs, and dynamic hooks are later external
  capabilities.

## Core Boundary

The kernel owns only:

- run lifecycle
- append-only event log
- SQLite-backed session, run, ingress, and journal records
- model provider interface
- invocation intent approval and adapter dispatch
- minimal audit records and health signals

The kernel does not own:

- multi-agent orchestration
- workflow graph engines
- long-term memory trees
- dashboards
- eval platforms
- deployment systems
- product-specific integrations beyond the first Feishu plugin

Those features should live as plugins, external services, scripts, or agent-written
programs that call the kernel through stable APIs.

The TypeScript side is only the Feishu edge adapter: long-connection auth,
event normalization, and `feishu.send_message` execution. It must not grow a
second Session, Context, Policy, LLM, Agent Loop, Gateway, or Journal.
It is kept in this repository for Phase 0 delivery speed, but its long-term
shape is an external plugin or independently developed connector that speaks the
kernel IPC contract.

## Architecture Status

- **Current implementation/governance baseline (V1)**: [`AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1.md`](docs/architecture/AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1.md) plus the current code. Statements like "kernel owns run/session" above describe the current V1 implementation — not necessarily final V2.1 ownership.
- **Architecture evolution discussion (V2.1)**: [`AGENT_CORE_V2_1_ARCHITECTURE_DISCUSSION_MAP.md`](docs/architecture/AGENT_CORE_V2_1_ARCHITECTURE_DISCUSSION_MAP.md) — still `WORKING DRAFT`; it has not replaced V1.
- Do not treat V1 implementation facts as final long-term ownership; do not treat the V2.1 target state as already implemented. Discussion may challenge V1 ownership, but development must not treat the draft as effective architecture until V2.1 is explicitly frozen.

## Documents

- [Architecture RFC](docs/architecture-rfc.md)
- [Milestones](docs/milestones.md)
- [Phase 0 Plan](docs/phase0-plan.md)
- [Design Doc](docs/design-doc.html)

Architecture drafts (under `docs/architecture/`):

- [V2.1 Architecture Discussion Map](docs/architecture/AGENT_CORE_V2_1_ARCHITECTURE_DISCUSSION_MAP.md) — WORKING DRAFT: current V2.1 evolution discussion entry (not yet effective architecture)
- [Kernel Primitive Calculus](docs/architecture/kernel-primitive-calculus.md) — draft candidate primitive model (not a refactor plan)
- [Primitive Screening Matrix](docs/architecture/primitive-screening-matrix.md) — per-concept code evidence and classification
- [Generic Self-Evolution V1](docs/architecture/generic-self-evolution-v1.md) — governed DevelopmentRequest, external profile Acceptance Receipts, Proposal/Approval, and historical HCR read compatibility
- [Model Invocation Telemetry V0](docs/architecture/model-invocation-telemetry-v0.md) — replay-safe Journal facts for real model usage through `event.observe.v0`
- [Managed Service Lifecycle V0](docs/architecture/managed-service-lifecycle-v0.md) — external Hook Consumer Service deployment, component snapshots, and governed controls
- [External Orchestration Boundary](docs/architecture/external-orchestration-boundary.md) — Kernel vs External Harness boundary: planning, routing, multi-agent, acceptance, and repair strategy all live outside the Kernel
- [External Harness Boundary V1](docs/architecture/AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1.md) — frozen Run-outcome/continuation boundary, `run.outcome.resolve.v0`, hook call direction, and Bootstrap → Self-hosting phases (same-session auto-continuation V0)
- [Deployment Harness Operations](docs/ops/deployment-harness.md) — loopback deployment service configuration, health, upgrade, disable, and rollback
- [Request-driven Coding Generator](docs/ops/request-driven-coding-generator.md) — model-generated Hook Consumer candidates, isolated repair probes, and operational boundaries

## Current Commands

```bash
pnpm check
pnpm agent-core run --text "hello" --json
pnpm agent-core serve
pnpm feishu-connector
cargo test
```

Runtime data defaults to `~/.agent-core`. The source repository owns code and
bootstrap defaults; local agent documents, `kernel.sqlite`, and connector-local
reaction state live outside the checkout by default.

## Feishu M1

M1 uses local IPC:

```text
Feishu long connection
-> TypeScript Connector
-> Rust Kernel /v1/ingress
-> Runtime + LLM
-> Connector /v1/execute
-> Feishu reply
```

Both local services bind to `127.0.0.1` and require `AGENT_CORE_IPC_TOKEN`.
Start the Rust Kernel first, then the connector:

```bash
pnpm agent-core serve
pnpm feishu-connector
```

## Key Principle

If a feature can be implemented as a plugin or an external loop, it should not be
implemented inside `core`. The core records facts, enforces boundaries, and
offers a small set of durable operations. Everything else grows outside.
