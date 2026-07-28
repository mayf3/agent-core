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

## Runtime state and deployment freeze (operator clarification)

The rules above freeze the *structure* of `~/.agent-core/`. The following
clarifies how that structure maps to the **current** deployment, so operators
and external Harnesses do not invent parallel directory hierarchies. This
section describes what exists today; it does not add a second long-term root.

1. **Single runtime root remains `~/.agent-core/`.** All Kernel data,
   Connector operational state, and registry snapshots live under this one
   root. There is no second long-term root, and no `releases/kernel/<head>/<digest>`
   directory convention has been frozen — such paths are treated as
   unconfirmed deployment experiments, not an authoritative layout.

2. **Kernel data directory today.** The live Journal / registry database in
   the current deployment is the Kernel-managed SQLite file under the active
   run root. Because the **final on-disk location of the Kernel data
   directory has not yet been frozen** (the proposed `data/agent-core.db`
   tree below is aspirational and unimplemented), the operator must follow
   whichever path the authoritative Operating Guide names as the current
   standard start path. Inventing or migrating to a new long-term path is a
   **user decision**, not an operator or Harness decision.

3. **Kernel start entry point.** The Kernel is started from the repository
   build of `agent-core-kernel` (a `cargo build --release` product), invoked
   as `agent-core-kernel serve --db <kernel_data_dir>`. Binaries copied into
   scratch, canary, or backup trees are not authoritative sources. See the
   Operating Guide for the canonical command and port.

4. **External Harness ownership boundary.** External Harness source, build
   artifacts, and per-process runtime configuration are owned by the
   external Harness itself (Rule 10), **not** by the Kernel and **not** by
   this decision document. Their concrete directories are an external Harness
   concern and are recorded in `docs/ops/deployment-harness.md`. The Kernel
   holds only endpoint/manifest references to them.

5. **HCR / Canary are verification environments, not the run root.** The
   `ops/hcr-linux-vm/` Lima VM, the `ops/canary-runtime/` scripts, and any
   `~/.agent-core/hcr-linux/...` tree are a **cleanable verification /
   canary environment**. They are not the long-term run root, and any data
   under them may be removed once a deployment is retired. (Note: the string
   "HCR" is overloaded — in `docs/architecture/` it means *Harness Change
   Request*, a Kernel domain concept; in `ops/` it means the *HCR Linux VM*.
   This document uses it only in the Kernel-domain sense elsewhere.)

6. **Ordinary Agents never write the Journal / registry directly.** Only the
   Kernel process appends to `journal_events`, `registry_state`,
   `registry_snapshots`, `component_registry_*`, and `harness_manifests`.
   Any direct write to the Kernel SQLite database by an ordinary Agent or
   Harness is a Rule-4 / Rule-8 violation.

7. **Stop conditions for recovery / upgrade / rollback.**
   - Recovery is complete when Kernel `/health.status` returns `ok` with
     `hash_chain_ok=true`, `outbox_unknown_count=0`,
     `projection_drift_count=0`, no pending/undelivered ingress, and the
     active registry snapshot id, context manifest id, and context artifact
     digest are byte-identical to the pre-recovery values.
   - An upgrade or rollback is complete when the same `/health` invariants
     hold **and** the deployment receipt in the Journal matches the rolled
     component.
   - If any of these cannot be satisfied, stop and report a Blocker rather
     than editing Journal rows.

## Deferred items (explicitly not implemented)

- Multi-Agent runtime loading and lifecycle management
- Cross-Agent routing (only single-Agent routing is supported)
- Isolation enforcement at the filesystem/process level
- `session.spawn` and `event.deliver` primitives
- Agent-level resource quotas (CPU, memory, rate limits)
- Secret provider integration (keychain, vault, etc.)
