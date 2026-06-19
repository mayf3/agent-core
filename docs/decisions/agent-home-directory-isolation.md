# Decision: Agent home-directory structure and isolation rules

Freezes the Agent home-directory layout, isolation boundaries, and identity
model. Rules established here constrain Phase 2+3 implementation.
Multi-Agent runtime loading, routing, isolation enforcement, spawn, and
delegation are explicitly deferred.

## Rules

1. **Single runtime root.** `~/.agent-core/` is the only root. No
   `~/.agentcore`, `~/.agenthome`, or other alias.

2. **Per-Agent directory.** Each Agent owns
   `~/.agent-core/agents/{agent_id}/` containing:
   - `agent.toml` — manifest (identity, model ref, enabled Skills, operation
     grants, context limits, workspace policy)
   - `AGENT.md` — system prompt fragment
   - `skills/` — private Skills (not shared with other Agents)
   - `workspace/` — restricted working directory

3. **Shared Skills.** `~/.agent-core/skills/` holds shared Skills. An Agent
   must explicitly enable a shared Skill in its `agent.toml`; enablement does
   not grant automatic cross-Agent data access.

4. **Shared infrastructure.** Kernel config, global SQLite (journal, outbox,
   approval state), Connector state, and logs live at the shared root. There
   is not one Kernel/Journal/Connector/schema per Agent.

5. **Agent identity in every record.** `agent_id` is part of Session, Run,
   and RunPrincipal identity. Session keys are namespaced, e.g.
   `agent:{agent_id}:feishu:dm:{open_id}`.

6. **agent.toml is the authority.** Agent identity, model reference, enabled
   Skills, operation grants, context limits, and workspace policy come from
   `agent.toml`. The Connector supplies source identity only; it never
   selects Skills or runs an Agent.

7. **Routing table.** `routes.toml` maps explicit
   `source/connector/chat_identity` tuples to `agent_id`.

8. **Default deny across Agents.** Private Sessions, Context, workspace,
   Skills, and Journal views are denied across Agents by default. Future
   cooperation uses explicit `session.spawn` and `event.deliver` — never
   direct file or database access.

9. **Credentials are references, not values.** Manifests contain credential
   references only. Secret values come from an OS keychain, environment
   injection, or a separate secret provider; they are never model-readable.

10. **Harness stays external.** Harness code remains outside `src/` and
    outside `~/.agent-core/`. `~/.agent-core/` may contain harness endpoint
    or command configuration, but never imported harness source.

11. **Repository content is example/test data only.** Agent/Skill content
    committed in this repository is example or test-fixture data, not the
    user's live runtime data.

12. **Multi-Agent is future work.** Full multi-Agent loading, routing,
    isolation enforcement, `session.spawn`, and `session.deliver` are
    explicitly not implemented now. This document establishes the boundary
    they must respect when implemented.

## Proposed directory tree

```
~/.agent-core/
  config.toml               # Shared global config
  routes.toml               # Source identity -> agent_id mapping
  agent-core.db             # Global SQLite (journal, outbox, approval, runs)
  agents/
    {agent_id}/
      agent.toml            # Identity, model, skills, grants, limits
      AGENT.md              # System prompt fragment
      skills/               # Private skills (not shared)
      workspace/            # Restricted working directory
  skills/                   # Shared Skills (explicit enablement required)
    {skill_name}/
      SKILL.md
      src/
  connectors/               # Connector state snapshots (read-only)
    feishu/
      execute-store.jsonl
  logs/
```

## Example `agent.toml`

```toml
[agent]
id = "assistant-alpha"
model = "claude-sonnet-4-20250514"

[grants]
operations = ["time.now", "feishu.send_message", "stdout.send_text"]
context_limit = 128000

[skills]
shared = ["web-search", "calculator"]
private = []

[workspace]
max_storage_mb = 100
allowed_paths = ["~/agent-core/agents/assistant-alpha/workspace/"]
```

## Example `AGENT.md`

```
You are assistant-alpha. You have access to:
- time.now to check the current time
- feishu.send_message to reply via Feishu
- stdout.send_text for debug output
- Shared Skills: web-search, calculator

You cannot read other Agents' sessions or workspace files.
Never ask for or read credentials; they are configured externally.
```

## Example `routes.toml`

```toml
[routes."feishu:dm:u_abc123"]
agent_id = "assistant-alpha"

[routes."cli:stdin:local"]
agent_id = "assistant-alpha"
```

## Deferred items (explicitly not implemented)

- Multi-Agent runtime loading and lifecycle management
- Cross-Agent routing (only single-Agent routing is supported)
- Isolation enforcement at the filesystem/process level
- `session.spawn` and `session.deliver` primitives
- Agent-level resource quotas (CPU, memory, rate limits)
- Secret provider integration (keychain, vault, etc.)
