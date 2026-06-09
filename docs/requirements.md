# Agent Core Requirements and Architecture

## 1. Goal

Build a minimal agent kernel that can run local tasks, expose safe tools, record
enough harness facts for debugging, and let external capabilities grow around it.

The design follows two ideas from recent agent-harness work:

- Reliability often comes from the harness around the model, not only the model.
- A harness should stay inspectable and removable. As models and tasks change,
  external loops should be easy to add, test, replace, or delete.

## 2. Product Shape

Agent Core should feel like a local daemon with a thin protocol surface:

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

The first user path is:

```text
Feishu message from phone
  -> Feishu plugin
  -> local agent run
  -> status updates
  -> final Feishu reply
```

## 3. Non-Goals

Do not put these into the kernel:

- multi-agent orchestration
- workflow DAG engine
- graphical dashboard
- long-term memory graph
- eval platform
- plugin marketplace
- automatic deployment pipeline
- large built-in integration catalog

Each may exist later as a plugin, external service, or agent-written program.

## 4. Kernel Primitives

The core package should expose only these primitives.

### 4.1 Run

A run is one unit of work.

Required fields:

```text
runId
source
userId
sessionId
status
inputSummary
createdAt
updatedAt
resultSummary
```

### 4.2 Event

Every meaningful action is an append-only event.

Common event types:

```text
run.started
run.completed
run.failed
model.called
model.completed
tool.called
tool.completed
approval.requested
approval.decided
artifact.created
```

### 4.3 Tool Registry

Tools declare:

```text
name
description
inputSchema
permission
timeoutMs
maxOutputBytes
handler
```

The model only sees tools allowed by policy for the current run.

### 4.4 Model Provider

Providers implement one interface:

```text
generate(messages, tools, options) -> assistant message or tool calls
```

Version 1 should support OpenAI-compatible APIs through environment config.

### 4.5 State Store

The first store may be JSONL or SQLite. The required logical records are:

```text
runs
events
model_calls
tool_calls
approvals
artifacts
context_snapshots
```

### 4.6 Approval Gate

Dangerous, persistent, or high-impact actions pause the run and request approval.

Approval records include:

```text
approvalId
runId
requestedAction
riskLevel
reason
policyVersion
decision
decidedBy
```

### 4.7 Plugin Registry

Plugins declare capabilities in a manifest and register them through a narrow API.

Plugin types:

```text
transport      Feishu, CLI, web, future chat channels
provider       OpenAI-compatible, local model, vendor adapters
capability     tools, search, browser, deployment, memory
orchestrator   workflow, multi-agent, planner/evaluator loops
```

### 4.8 Execution Boundary and Sandbox

Sandboxing is valuable, but the first version should not start with a large
container orchestration system inside the kernel.

Version 1 boundary:

```text
workspace allowlist
cwd containment
command timeout
stdout and stderr cap
environment redaction
network policy flag
approval for write, execute, and dangerous actions
```

The kernel records enough execution metadata for replay:

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

Later sandbox adapters can be external capabilities:

```text
local process sandbox
Docker sandbox
remote sandbox service
ephemeral worktree
record/replay runner
```

The kernel should expose a sandbox provider interface, but concrete sandbox
implementations should live outside `core`.

## 5. Dynamic External Capability Model

Yes, this architecture can support the MOSS-like pattern where many features are
implemented outside the kernel and iterated by agents themselves.

The key is that dynamic loading does not mean arbitrary unreviewed code inside
`core`. It means controlled capability discovery and activation.

### 5.1 External Loop Contract

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

### 5.2 Plugin Loading Stages

Version 1 should use explicit loading:

```text
discover manifest
  -> validate schema
  -> show capabilities
  -> require enable or approval
  -> register tools/providers/transports
```

Dynamic stages can grow carefully:

```text
v0: load trusted local plugins at startup
v1: enable or disable out-of-process plugins without restarting
v2: hot reload trusted development plugins
```

Untrusted or multi-language capabilities should run out of process through one of:

```text
child process protocol
HTTP local service
MCP server
message queue
```

### 5.3 Agent-Written Extensions

The agent may write scripts, services, workflows, or plugins in the workspace.

Activation flow:

```text
agent writes extension
  -> agent runs local checks
  -> core records artifact
  -> approval requested for enablement
  -> plugin manifest validated
  -> capability becomes available
```

This gives source-level evolution without turning the kernel into a large
platform.

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

The Feishu plugin should translate Feishu events into generic core events. The
core should not know Feishu-specific message shapes.

## 7. Built-In Tools

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

## 8. Minimal Agent Loop

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
just an orchestrator plugin that creates and observes multiple runs.

## 9. Harness Records

The kernel records enough to answer:

- What did the user ask?
- Which model and prompt version were used?
- Which context was exposed?
- Which tools were called?
- Which policy decisions were made?
- What changed in the workspace?
- Why did the run fail or pause?
- What should an external eval or regression test replay?

Do not store full sensitive content by default. Prefer summaries, hashes, and
redacted payloads unless debug mode is explicitly enabled.

## 10. Milestones

### M0: Documentation and Project Skeleton

- README
- requirements document
- engineering constraints
- pnpm workspace
- package boundaries

### M1: Kernel Records

- run store
- event store
- envelope
- state path
- doctor command

### M2: Tools and Approval

- built-in tools
- policy decisions
- approval pause and resume
- audit records

### M3: Model and Agent Loop

- OpenAI-compatible provider
- single-agent loop
- tool call dispatch
- transcript and context snapshot

### M4: Feishu Plugin

- Feishu receive and send
- session mapping
- mobile approval commands
- progress updates

### M5: External Extension Loop

- plugin manifest
- out-of-process capability adapter
- event subscription
- agent-written extension approval flow
