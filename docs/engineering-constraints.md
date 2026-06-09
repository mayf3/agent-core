# Engineering Constraints

These constraints keep Agent Core small, inspectable, and easy for an agent to
modify safely.

## 1. Size Limits

Hard limits for authored source files:

```text
single source file: <= 500 lines
single directory: <= 20 files
general directory depth: <= 4 levels
Node.js workspace directory depth: <= 6 levels
```

Authored Markdown and config files should also stay under 500 lines unless there
is a clear reason. Generated files should be isolated and not mixed with source.

## 2. Directory Discipline

Each directory should represent one concept family.

Good examples:

```text
packages/core/src/run
packages/core/src/events
packages/tools/src/builtins
packages/plugins/feishu/src
```

Avoid directories that mix unrelated concepts such as tools, UI, providers, and
state code together.

## 3. Package Boundaries

Planned package responsibilities:

```text
packages/core       run, event, state, envelope, approval, registry
packages/tools      built-in tools and policy helpers
packages/agent      single agent loop and context assembly
packages/providers  model provider adapters
packages/plugins    transport and capability plugins
packages/cli        command-line access layer
```

Rules:

- `core` must not import `cli`, `agent`, or a concrete plugin.
- `agent` must call tools only through the tool registry.
- plugins must register capabilities through the plugin API.
- transport plugins must translate external messages into generic core events.
- provider adapters must not know about Feishu, CLI, or workflow engines.

## 4. Thin Core Rule

Before adding anything to `core`, ask:

```text
Can this be a plugin?
Can this be an external process?
Can this be a tool?
Can this be represented as events plus state?
```

If yes, it stays outside `core`.

Core is allowed to own:

- stable contracts
- persistence of required records
- policy and approval enforcement
- capability registration
- run lifecycle

Core is not allowed to own:

- business workflows
- multi-agent coordination
- external product APIs except through plugin interfaces
- UI state
- evaluation logic
- deployment logic

## 5. Dynamic Loading Rule

Dynamic loading must be explicit and auditable.

Required flow:

```text
discover
validate
show capabilities
approve or enable
register
record event
```

No plugin may:

- override a core tool
- mutate the state store directly
- add model-visible tools without policy evaluation
- read secrets outside declared config
- execute install scripts silently

## 6. Record-First Development

Every meaningful operation should emit a structured event before and after it
runs.

Minimum event coverage:

```text
run lifecycle
model calls
tool calls
approval requests
approval decisions
plugin loading
artifact creation
policy denial
errors
```

This is how external dashboards, evals, workflow engines, and multi-agent systems
can be built without entering the kernel.

## 7. Security and Privacy

The repository must be safe to push to a personal remote.

Rules:

- never commit `.env` files
- never commit API keys or Feishu secrets
- never commit local state or raw sensitive logs
- record config readiness, not secret values
- prefer redacted previews, hashes, and ids in logs
- keep debug artifacts ignored by git

Every PR should run the local secret scan.

## 8. Testing and Checks

The project should eventually provide automated checks for:

- file line limits
- directory file count
- directory depth
- package import boundaries
- generated file freshness
- local secret leakage
- basic unit tests

The first check command should be:

```text
pnpm check
```

## 9. Style

- Prefer small modules over large classes.
- Prefer structured records over ad hoc strings.
- Prefer explicit schemas at boundaries.
- Keep comments short and useful.
- Do not add framework abstractions before the use case exists.
- Keep plugin APIs narrow and versioned.
