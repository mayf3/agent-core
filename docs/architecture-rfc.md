# Agent Core Architecture RFC

This RFC records long-lived semantics. It is not a phase-one implementation
checklist. Protocols may be defined here before they are implemented.

## 1. Kernel Boundary

Agent Core is a small local-first kernel. It owns stable primitives:

- run lifecycle
- append-only events
- state records
- model provider interface
- tool contract and invocation dispatch
- approval state
- plugin and adapter registration contracts

Agent Core does not own product features:

- Feishu business behavior beyond the transport adapter
- workflow graphs
- multi-agent orchestration
- long-term memory systems
- dashboards
- eval platforms
- heavy sandbox infrastructure
- deployment systems

Those features are external harnesses, plugins, adapters, or agent-written
programs.

## 2. Current Kernel Modules

```text
src/domain.rs          durable domain types and ID newtypes
src/gateway            ingress validation and invocation approval
src/runtime            single-agent event delivery loop
src/journal            SQLite journal, hash chain, recovery scans
src/context.rs         Phase 0 file-backed context assembly
src/llm                OpenAI-compatible provider adapter
src/server             localhost HTTP kernel API and health
connectors/feishu      TypeScript Feishu edge adapter only
```

Future modules may be added only when repeated behavior proves a boundary.

## 3. Tool and Effect Semantics

Use these terms consistently:

| Term | Meaning |
|---|---|
| Tool Contract | Name, description, parameter schema, return shape shown to a model |
| Tool Intent | Model-proposed request; no side effect has happened yet |
| Policy / Hook | Local decision layer that may allow, deny, transform, or pause |
| Approval | Durable governance state created when execution is paused |
| Adapter | Code that performs the concrete external or local operation |
| Receipt / Result | Proof or output returned by the adapter |
| Audit | Append-only record of intent, decision, execution, and result |

The long-term invocation chain is:

```text
Intent
  -> Schema Validation
  -> Principal / Capability
  -> Policy / Hook
  -> Approval if needed
  -> Adapter
  -> Receipt or Result
  -> Audit
```

Reads can still be sensitive. The gateway must eventually cover both external
queries and commands, not only obvious side effects.

## 4. Run Principal

Permissions belong to a run, not only to an agent.

```ts
interface RunPrincipal {
  agentId: string;
  channel: "cli" | "feishu" | "api" | "cron";
  requesterId?: string;
  executionProfile: string;
  capabilities: CapabilityGrant[];
}
```

Examples:

| Entry | Default capability |
|---|---|
| Local CLI | read tools; write/execute require approval |
| Feishu private chat | chat only |
| Feishu group | chat only, mention required |
| Workflow event | resources scoped to the external task |

Feishu must not inherit local CLI tool permissions by default.

## 5. Hook Semantics

Full hook runtime is future work. The protocol is defined now to avoid unsafe
ordering later.

Hook execution order:

```text
System Pre
  -> Harness
  -> User
  -> System Final Guard
```

Why the final guard exists: user or project hooks may transform payloads after
earlier checks. Final system guard revalidates non-bypassable invariants before
execution.

Stable sorting:

```text
phase -> scope -> priority -> ordinal -> hook id
```

`ordinal` must be persisted in configuration or registry state; process startup
order is not a stable ordering contract.

Hook result semantics:

| Result | Behavior |
|---|---|
| pass | Continue |
| deny | Stop immediately |
| transform | Replace current payload and continue |
| require-approval | Persist suspended invocation and pause |

Approval resume must not continue from an in-memory JavaScript continuation.
Resume should reload state, validate request hash and versions, rerun mandatory
guards, then continue.

```ts
interface SuspendedInvocation {
  invocationId: string;
  hookPoint: string;
  payload: unknown;
  payloadHash: string;
  hookSetVersion: string;
  runId: string;
  expiresAt: number;
}
```

Implementation note: early phases use fixed policies/interceptors instead of a
dynamic hook registry.

## 6. External Systems

External systems register operations and events. They do not automatically inject
arbitrary code into the kernel.

```ts
interface ExternalSystemManifest {
  id: string;
  version: string;
  commands: OperationSpec[];
  queries: OperationSpec[];
  events: EventSpec[];
  authRef?: string;
  transport: "in-process" | "http" | "stdio" | "webhook";
  trustLevel: "system" | "trusted" | "untrusted";
}
```

```ts
interface OperationSpec {
  name: string;
  description: string;
  inputSchema: JsonSchema;
  outputSchema: JsonSchema;
  effect: "read" | "write" | "external-side-effect";
  idempotent: boolean;
}
```

Runtime adapter:

```ts
interface ExternalSystemAdapter {
  command(operation: string, input: unknown, ctx: InvocationContext): Promise<Receipt>;
  query(operation: string, input: unknown, ctx: InvocationContext): Promise<QueryResult>;
  startEventSubscription?(emit: (event: RuntimeEvent) => Promise<void>): Promise<Disposable>;
}
```

Trusted local runtime decides which hooks are registered. Untrusted systems may
offer declarative rules, not arbitrary in-process JavaScript.

## 7. Context Architecture

Context is produced by contributors, planned by a planner, and serialized for a
provider.

```ts
interface ContextContributor {
  id: string;
  kinds: ContextBlockKind[];
  scope: "system" | "agent" | "project" | "run";
  priority: number;
  build(ctx: BuildContext): Promise<ContextBlock[]>;
}
```

Context pipeline:

```text
contributors produce candidate blocks
  -> normalize
  -> permission and sensitivity filter
  -> dedupe and sort
  -> token budget allocation
  -> trim or compress
  -> provider-specific serialization
```

Root System is special:

- only system scope may provide it
- never overwritten by normal contributors
- never compressed by normal compressors
- must record source, version, and hash

Context block metadata:

```ts
interface ContextBlock {
  id: string;
  kind: ContextBlockKind;
  content: string;
  sourceRef: string;
  sourceVersion?: string;
  contentHash: string;
  priority: number;
  compressibility: "never" | "drop-whole" | "summarizable" | "truncate";
  sensitivity: "public" | "internal" | "secret";
  tokenEstimate?: number;
}
```

Early phases may use simple prompt assembly, but they should not violate these
future invariants.

## 8. Self-Modification

MOSS-style self-evolution is allowed only through bounded source-level workflow:

```text
Observe failure
  -> Attribute cause
  -> Propose patch
  -> Candidate branch or worktree
  -> Static checks
  -> Replay selected historical runs
  -> Evaluate into score/report
  -> Human or policy approval
  -> Promote by PR merge and tag
  -> Rollback to last-known-good if needed
```

No agent may bypass tool policy, approval, checks, or repository workflow while
modifying `agent-core`.

## 9. Storage Direction

SQLite is used from the first CLI message. The smallest durable surface is:

```text
sessions        conversation state
runs            processing state
ingress_dedup   external event/message idempotency
journal_events  append-only facts with hash chain and correlation IDs
```

Do not promise strict exactly-once delivery across Feishu and local storage.
Target: at-least-once receive plus local idempotency and best-effort duplicate
reply prevention.

## 10. Security Baseline

- secrets never enter git, model context, or ordinary logs
- Feishu default profile is chat-only
- group messages require mention unless explicitly configured otherwise
- shell and writes require approval
- approval resume rechecks policy and request identity
- external systems cannot expand model-visible tools silently
