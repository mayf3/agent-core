# Agent Core Requirements

This is the single source of truth for Agent Core requirements, architecture,
engineering constraints, security rules, and repository workflow.

## 1. Goal

Build a minimal local-first agent kernel that can run tasks, expose safe tools,
record enough harness facts for debugging and replay, and let external
capabilities grow around it. Most product features should be plugins, external
services, scripts, or agent-written programs.

## 2. Product Shape

Agent Core behaves like a local daemon with a thin protocol surface:

```text
user message
  -> transport plugin
  -> core run
  -> agent loop
  -> model provider
  -> tool dispatch
  -> approval when needed
  -> event log and result
  -> transport reply
```

The first user-visible path is Feishu:

```text
Feishu message from phone
  -> Feishu plugin
  -> local agent run
  -> progress updates
  -> final Feishu reply
```

## 3. Non-Goals

Do not put these into the kernel:

- orchestration: multi-agent routing, workflow DAGs, eval platforms
- product surfaces: dashboards, marketplaces, large integration catalogs
- heavy infrastructure: deployment pipelines, memory graphs, Docker or remote sandbox orchestration

Each may exist later as a plugin, external adapter, local service, or
agent-written program.

## 4. Kernel Primitives

### 4.1 Run

A run is one unit of work.

```text
runId, source, userId, sessionId, status, inputSummary, createdAt, updatedAt, resultSummary
```

### 4.2 Event

Every meaningful action is an append-only event.

```text
run.started, run.completed, run.failed, model.called, model.completed,
tool.called, tool.completed, approval.requested, approval.decided,
artifact.created, plugin.loaded, policy.denied
```

### 4.3 Tool Registry

Tools declare:

```text
name, description, inputSchema, permission, timeoutMs, maxOutputBytes, handler
```

The model only sees tools allowed by policy for the current run.

### 4.4 Model Provider

Providers implement one interface:

```text
generate(messages, tools, options) -> assistant message or tool calls
```

Version 1 supports OpenAI-compatible APIs through local environment config.

### 4.5 State Store

The first store may be JSONL or SQLite. Required logical records:

```text
runs, events, model_calls, tool_calls, approvals, artifacts, context_snapshots
```

### 4.6 Approval Gate

Dangerous, persistent, or high-impact actions pause the run and request approval.

```text
approvalId, runId, requestedAction, riskLevel, reason, policyVersion, decision, decidedBy
```

### 4.7 Plugin Registry

Plugins declare capabilities in a manifest and register through a narrow API.

Plugin types:

```text
transport      Feishu, CLI, web, future chat channels
provider       OpenAI-compatible, local model, vendor adapters
capability     tools, search, browser, deployment, memory
orchestrator   workflow, multi-agent, planner/evaluator loops
```

## 5. Dynamic External Capability Model

This architecture supports MOSS-like source-level evolution without turning the
kernel into a large platform.

Dynamic loading means controlled discovery and activation:

```text
discover manifest
  -> validate schema
  -> show capabilities
  -> approve or enable
  -> register
  -> record event
```

An external loop can:

- subscribe to events
- create runs
- read run status
- invoke allowed tools
- request approvals
- register artifacts
- emit status and result events

An external loop cannot:

- mutate core state directly
- bypass approval
- override built-in tools
- silently expand model-visible permissions
- load secrets without explicit config

Agent-written extensions follow this path:

```text
agent writes extension
  -> agent runs local checks
  -> core records artifact
  -> approval requested for enablement
  -> plugin manifest validated
  -> capability becomes available
```

Untrusted or multi-language capabilities should run out of process through:

```text
child process protocol
HTTP local service
MCP server
message queue
```

## 6. Feishu Version 1

Feishu is the first transport plugin because it gives a practical mobile control
surface.

Minimum features:

- receive direct messages and group mentions
- deduplicate message events
- map chat to session
- send immediate acknowledgement
- send progress updates
- send final result
- support approval commands

Initial commands:

```text
/status
/stop
/approve <id>
/reject <id>
/runs
```

The Feishu plugin translates Feishu events into generic core events. The core
must not know Feishu-specific message shapes.

Feishu secrets stay in local environment variables or a local secret store. They
must never be committed.

## 7. Sandbox Strategy

Sandboxing is important, but a heavy sandbox system inside the kernel would make
version 1 too large.

Version 1 uses a lightweight execution boundary:

```text
workspace allowlist
cwd containment
command timeout
stdout and stderr cap
environment redaction
network policy flag
approval for write, execute, and dangerous actions
```

The kernel records enough metadata for later replay:

```text
workspace
cwd
command
tool version
env profile
timeout
allowed paths
policy decision
exit code
output hash
```

Heavier sandboxes are external adapters:

```text
local process sandbox
Docker sandbox
remote sandbox service
ephemeral worktree
record/replay runner
```

The kernel exposes a sandbox provider interface only when a real adapter needs
it. Docker orchestration and replay infrastructure stay outside `core`.

## 8. Built-In Tools

Version 1 built-ins:

```text
fs.read
fs.write
fs.list
search.grep
shell.exec
http.fetch
state.read
approval.request
```

Each tool must declare permission level and output limits.

## 9. Minimal Agent Loop

Version 1 supports one agent loop:

```text
build context
  -> call model
  -> handle tool calls
  -> dispatch tools through policy
  -> append tool results
  -> stop on final answer, error, budget, or approval
```

No multi-agent orchestration is required in the kernel. A multi-agent system is
an orchestrator plugin that creates and observes multiple runs.

## 10. Harness Records

The kernel records enough to answer:

- What did the user ask?
- Which model and prompt version were used?
- Which context was exposed?
- Which tools were called?
- Which policy decisions were made?
- What changed in the workspace?
- Why did the run fail or pause?
- What should an external eval or regression test replay?

Do not store full sensitive content by default. Prefer summaries, hashes, ids,
and redacted payloads unless debug mode is explicitly enabled.

## 11. Engineering Constraints

Size limits:

```text
single source file: <= 500 lines
single directory: <= 20 files
general directory depth: <= 4 levels
Node.js workspace directory depth: <= 6 levels
```

Directory rule:

```text
each directory represents one concept family
```

Planned package responsibilities:

```text
packages/core       run, event, state, envelope, approval, registry
packages/tools      built-in tools and policy helpers
packages/agent      single agent loop and context assembly
packages/providers  model provider adapters
packages/plugins    transport and capability plugins
packages/cli        command-line access layer
```

Dependency rules:

- `core` must not import `cli`, `agent`, or concrete plugins.
- `agent` must call tools only through the tool registry.
- plugins must register capabilities through the plugin API.
- transport plugins must translate external messages into generic core events.
- provider adapters must not know about Feishu, CLI, or workflow engines.

Before adding anything to `core`, ask:

```text
Can this be a plugin?
Can this be an external process?
Can this be a tool?
Can this be represented as events plus state?
```

If yes, it stays outside `core`.

## 12. Security and Privacy

Treat the repository as public by default.

Never commit:

- API keys
- Feishu app secrets
- access tokens
- private keys
- `.env` files
- local run state
- logs containing prompts, tool outputs, or user data
- production config with real credentials

Allowed records:

```text
FEISHU_APP_ID is configured
OPENAI_BASE_URL is configured
provider=openai-compatible
```

Forbidden records:

```text
FEISHU_APP_SECRET=...
OPENAI_API_KEY=...
Authorization: Bearer ...
```

Default logs store summaries, hashes, ids, and redacted previews. Full prompts,
tool outputs, and external messages require explicit debug mode and must stay
ignored by git.

## 13. Repository Workflow

The repository uses PR-first history after bootstrap.

```text
create branch
  -> make changes
  -> run pnpm check
  -> commit
  -> push branch
  -> open PR
  -> merge PR
```

Reviews are optional for now, but PRs are required for traceability once a remote
repository exists.

Each PR records:

```text
problem
decision
files changed
checks run
known risks
next step
```

Direct commits to `main` are acceptable only for local bootstrap before the
remote exists.

## 14. Checks

The first check command is:

```text
pnpm check
```

It currently covers:

- file line limits
- directory file count
- directory depth
- basic local secret pattern scan

The secret scan is a guardrail, not a complete DLP system.

## 15. Milestones

| Phase | Scope | Deliverables |
|---|---|---|
| M0 | Documentation and skeleton | README, single requirements document, design review HTML, `pnpm check`, git repo |
| M1 | Kernel records | run store, event store, envelope, state path, doctor command |
| M2 | Tools, sandbox boundary, approval | built-in tools, lightweight execution boundary, policy decisions, pause/resume, audit records |
| M3 | Model and agent loop | OpenAI-compatible provider, single-agent loop, tool call dispatch, transcript, context snapshot |
| M4 | Feishu plugin | receive/send, session mapping, mobile approval commands, progress updates |
| M5 | External extension loop | plugin manifest, out-of-process adapter, event subscription, agent-written extension approval flow |
