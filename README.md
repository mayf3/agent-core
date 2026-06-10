# Agent Core

Agent Core is a small local-first agent kernel. Its job is not to become a full
agent platform. Its job is to provide stable primitives so agents can build,
load, and improve external capabilities around it.

## Direction

- Runtime: Node.js, TypeScript, ESM, pnpm workspace.
- Kernel style: small core with explicit extension points.
- First usable channel: Feishu, so mobile messages can start and steer local runs.
- First model interface: OpenAI-compatible provider.
- First tools: filesystem, shell, HTTP, state, and approval.

## Core Boundary

The kernel owns only:

- run lifecycle
- append-only event log
- state store
- tool registry and dispatch
- model provider interface
- plugin registry
- approval gate
- minimal audit records

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

## Documents

- [Architecture RFC](docs/architecture-rfc.md)
- [Milestones](docs/milestones.md)
- [Phase 0 Plan](docs/phase0-plan.md)
- [Design Doc](docs/design-doc.html)

## Current Commands

```bash
pnpm check
pnpm agent-core run --text "hello" --json
pnpm agent-core serve
pnpm feishu-connector
cargo test
```

The official Runtime is now the Rust Kernel. Existing Node packages are prototype
reference code for later TypeScript Feishu Connector extraction; they are not
the active Runtime, Gateway, or Journal.

## Feishu M1

M1 uses local IPC:

```text
Feishu long connection
-> TypeScript Connector
-> Rust Kernel /v1/ingress
-> fixed echo Invocation
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
