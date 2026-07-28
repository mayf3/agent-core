# Decision: Agent home-directory structure and isolation rules

Freezes the Agent home-directory layout, isolation boundaries, and identity
model for future implementation. Multi-Agent runtime loading, routing,
isolation enforcement, spawn, and delegation are explicitly deferred;
Phase 2 and Phase 3 deliverables do not include them.

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
   approval state), Connector operational state, and logs live at the shared
   root. There is not one Kernel/Journal/Connector/schema per Agent.

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
   direct file or database access. Note: the primitive is `event.deliver`,
   not `session.deliver`.

9. **Credentials are references, not values.** Manifests contain credential
   references only. Secret values come from an OS keychain, environment
   injection, or a separate secret provider; they are never model-readable.

10. **Harness stays external.** Harness code remains outside `src/` and
    outside `~/.agent-core/`. `~/.agent-core/` may contain harness endpoint
    or command configuration, but never imported harness source.
    Harnesses may temporarily exist under repository `tools/` during active
    development, but the final product boundary is a separate repository,
    package, or process. Kernel `src/` and `~/.agent-core/` must never import
    or contain harness source code; the runtime root holds only endpoint or
    command references.

11. **Repository content is example/test data only.** Agent/Skill content
    committed in this repository is example or test-fixture data, not the
    user's live runtime data.

12. **Multi-Agent is future work.** Full multi-Agent loading, routing,
    isolation enforcement, `session.spawn`, and `event.deliver` are
    explicitly not implemented now. This document establishes the boundary
    they must respect when implemented. Phase 2 and Phase 3 deliverables
    do not include multi-Agent isolation; that remains a future invariant.

## Proposed directory tree

```
~/.agent-core/
  config.toml               # Shared global config
  routes.toml               # Source identity -> agent_id mapping
  data/
    agent-core.db           # Global SQLite (journal, outbox, approval, runs)
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
  state/
    connector/              # Connector operational state (implementation-defined)
  logs/
```

## Example `agent.toml`

```toml
[agent]
id = "assistant-alpha"
display_name = "Assistant Alpha"
model = "deepseek-v4"
system_profile = "default"
credential_ref = "llm.deepseek.default"
enabled_skills = ["shared:system-status", "private:coding-handoff"]

[grants]
operations = ["time.now", "feishu.send_message", "stdout.send_text"]

[limits]
context_window = 128000

[workspace]
max_storage_mb = 100
allowed_paths = ["~/.agent-core/agents/assistant-alpha/workspace/"]
```

## Example `AGENT.md`

```
You are assistant-alpha. You have access to:
- time.now to check the current time
- feishu.send_message to reply via Feishu
- stdout.send_text for debug output
- enabled_skills: shared:system-status, private:coding-handoff

You cannot read other Agents' sessions or workspace files.
Never ask for or read credentials; they are configured externally.
```

## Example `routes.toml`

```toml
[routes."feishu:dm:open_id:ou_xxx"]
agent_id = "assistant-alpha"

[routes."cli:stdin:local"]
agent_id = "assistant-alpha"
```

## Final runtime state and deployment freeze

In this section, every `~` is the home directory of the runtime user **inside
the Linux VM**, not the macOS host home. `~/.agent-core/` is the only
long-term runtime root.

The following paths are frozen:

```text
~/.agent-core/
  data/
    agent-core.db
  runtime/
    kernel/
      agent-core-kernel
      provenance.json
  config/
    runtime.env
  logs/
    kernel.log
  run/
    kernel.pid
  deployment/
    artifacts/
    state/
```

1. **Kernel state has one permanent authority.** The Kernel database is
   `~/.agent-core/data/agent-core.db`. Names such as `kernel.sqlite` and
   `v2-canary.db` are not long-term authority. The database contains Journal,
   outbox, approval, run, and Registry state; there is no second local
   authority.

2. **Kernel has one fixed executable entry point.** Start the Kernel only
   from `~/.agent-core/runtime/kernel/agent-core-kernel`. An upgrade writes and
   verifies a temporary `agent-core-kernel.new`, atomically replaces the fixed
   entry point, and updates
   `~/.agent-core/runtime/kernel/provenance.json` to match the installed bytes.
   At most one temporary `agent-core-kernel.prev` may be retained for rollback.
   Per-HEAD or per-digest Kernel release directories, including
   `releases/kernel/<head>/<digest>`, are not part of the runtime contract.

3. **Kernel operational files are fixed.** Runtime configuration is
   `~/.agent-core/config/runtime.env`, the Kernel log is
   `~/.agent-core/logs/kernel.log`, and the PID file is
   `~/.agent-core/run/kernel.pid`.

4. **Deployment Harness roots are separate from Kernel state.** Deployed
   artifacts live under `~/.agent-core/deployment/artifacts/`; Deployment
   Harness state lives under `~/.agent-core/deployment/state/`. Neither root
   may overlap the Kernel database. Harness source and build trees remain
   outside `~/.agent-core/`.

5. **Repositories and verification trees are never runtime entry points.**
   Git worktrees, Cargo `target/`, HCR, Canary, SSHFS mounts, temporary build
   trees, shadow trees, and per-release staging directories are not
   authoritative runtime locations. HCR and Canary objects may be considered
   for cleanup only after every process and configuration reference has moved
   to the frozen paths and the final real Feishu smoke has passed.

6. **Ordinary Agents never write the Journal / Registry SQLite.** Only the
   Kernel process may append or mutate Kernel-owned SQLite state. Agents,
   Connectors, Providers, and Harnesses must use governed Kernel interfaces;
   direct writes to `journal_events`, Registry tables, or projections are
   prohibited.

7. **Recovery / upgrade / rollback stop conditions.** Completion requires all
   of the following:
   - Kernel `/health.status=ok`;
   - `hash_chain_ok=true`, `outbox_unknown_count=0`,
     `outbox_projection_drift_count=0`, and no pending or undelivered work;
   - active Registry snapshot id, context manifest id, and context artifact
     digest are byte-identical to their pre-operation values; and
   - a real Feishu tool round trip succeeds.

   If any condition cannot be satisfied, stop and report a Blocker. Never edit
   Journal or Registry rows to force the condition.

## Deferred items (explicitly not implemented)

- Multi-Agent runtime loading and lifecycle management
- Cross-Agent routing (only single-Agent routing is supported)
- Isolation enforcement at the filesystem/process level
- `session.spawn` and `event.deliver` primitives
- Agent-level resource quotas (CPU, memory, rate limits)
- Secret provider integration (keychain, vault, etc.)
