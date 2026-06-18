# Agent Dispatch Protocol

This document defines how to delegate implementation work to another coding
agent, including a cheap/high-throughput model such as DeepSeek V4 Flash.

The goal is to increase throughput without losing the thin-kernel boundary,
secret safety, or PR traceability.

## Roles

Use the stronger reviewing agent or maintainer for:

- architecture decisions;
- kernel boundary decisions;
- final PR review;
- security and duplicate-reply risk review;
- deciding when a phase is allowed to start.

Use the worker agent for:

- narrow implementation tasks;
- focused tests;
- doc synchronization;
- small refactors with unchanged behavior;
- read-only external harness prototypes.

The worker agent should not invent new architecture. It should implement one
assigned task and stop at a PR.

## Hard Rules

Every delegated task must follow these rules:

- Start from the real current `main`.
- Use one branch per task.
- Keep the task small enough to review in one PR.
- Do not read or commit `.env`, `.openduck`, `.openclaw`, logs, API keys,
  tokens, Authorization headers, or production SQLite data.
- Do not stop or restart local services unless the task explicitly says so.
- Do not add audit, replay, evaluator, workflow, multi-agent, memory, sandbox, or
  self-evolution implementation under `src/`.
- Keep Rust Kernel as the only Runtime / Gateway / Journal owner.
- Keep TypeScript Feishu Connector as an edge adapter.
- Finish with a PR, validation results, residual risks, and the next suggested
  task.

## Branch Flow

```text
git switch main
git pull --ff-only
git switch -c <branch-name>
implement only the assigned scope
run required validation
commit
push
open PR
report PR link + validation + risks
```

If the worker sees a dirty worktree before starting, it must stop and report the
dirty files. It must not overwrite another agent's work.

## Batch Review Mode

A GOL-style worker loop may open several PRs before the reviewing agent looks at
them. This is useful for throughput, but only under a small batch size and clear
stop rules.

Recommended batch size:

- 1 PR for architecture, schema, state-machine, approval, duplicate-reply, or
  security-sensitive changes.
- 2 to 3 PRs for independent docs, tests, fixtures, or narrow external harness
  work.
- Do not accumulate more than 3 open worker PRs without review.

The worker may continue autonomously only when the next PR is independent of
the previous unreviewed PR. If PR B depends on PR A, stop after PR A and wait for
review/merge.

Batch-safe examples:

- one doc-sync PR;
- one connector test coverage PR;
- one external harness design PR.

Not batch-safe examples:

- a schema design PR followed by an implementation that assumes that schema;
- a policy change followed by a tool execution change;
- an outbox/recovery change followed by a service startup change;
- any change that could affect duplicate replies.

## Codex Collaboration Points

If the worker can call Codex directly, use Codex as a reviewer/checkpoint, not
as a line-by-line pair programmer. The worker should avoid streaming every
small edit to Codex.

Call Codex at these checkpoints:

1. **Plan checkpoint** before starting a boundary-sensitive task. Ask whether
   the proposed scope violates the thin-kernel boundary.
2. **Schema checkpoint** before writing implementation that depends on SQLite
   table names, Journal event semantics, health fields, or status machines.
3. **Pre-PR checkpoint** after implementation and validation, with the PR diff
   summary and residual risks.
4. **Batch checkpoint** after 2 to 3 independent PRs, or sooner if one PR fails
   review.

Do not call Codex for:

- every file read;
- every small refactor step;
- routine formatting;
- test iteration that stays inside the assigned scope.

When calling Codex, provide:

```text
Task:
<one sentence>

Branch / PR:
<branch or PR URL>

Allowed scope:
<files/directories>

What changed:
<short bullet list>

Validation:
<commands and results>

Questions:
<specific review questions>
```

The worker should then continue only after recording the Codex feedback in the
PR or task handoff. If Codex identifies a blocking issue, fix that PR before
starting dependent work.

## Task Packet Template

Copy this block when assigning work to a worker agent:

```text
Read first:
- docs/current-goal.md
- docs/agent-dispatch.md
- any file listed in this task

Goal:
<one-sentence outcome>

Branch:
<branch-name>

Allowed scope:
<files/directories the agent may edit>

Forbidden:
- no .env / .openduck / .openclaw / logs / tokens / production DB
- no service stop/restart
- no new Kernel module unless explicitly listed
- no workflow/multi-agent/shell/memory/plugin/sandbox/self-evolution

Acceptance criteria:
- <observable behavior or doc state>
- <tests or checks>
- <no boundary violation>

Validation:
- <commands to run>

PR body must include:
- summary
- validation results
- files changed
- residual risks
- next suggested task
```

## Recommended Parallel Work

These tasks are safe to delegate because they are narrow and reviewable.

### Task A: Product Roadmap Drift Cleanup

Goal: update `docs/product-roadmap.md` so it matches current `main`.

Allowed scope:

- `docs/product-roadmap.md`
- optionally `docs/current-goal.md` if the task is merged and needs tracker
  refresh

Acceptance criteria:

- `RunStatus::Unknown` is no longer described as undecided.
- Phase 2 completion and ToolCatalog foundation are accurately reflected.
- No new implementation scope is added.

Validation:

```text
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs
git diff --check
```

### Task B: External Audit Harness Design

Goal: write a precise MVP design for a read-only audit report harness.

Allowed scope:

- new `docs/external-audit-harness.md`
- optionally `docs/current-goal.md` tracker refresh after merge

Acceptance criteria:

- The harness is explicitly outside the Rust Kernel and outside `src/`.
- Inputs are copied SQLite DB/snapshot, optional `/health` JSON, and Git
  revision.
- Outputs are `report.md` and `report.json`.
- The design lists exact sections: schema version, hash-chain status, recent
  runs, unknown dispatches, projection drift, undelivered ingress, approval
  waits/expiries, duplicate-reply safety notes.
- The design states that production DB, approval state, connector state, and
  Journal must not be mutated.

Validation:

```text
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs
git diff --check
```

### Task C: External Audit Harness MVP Prototype

Goal: add a read-only audit report prototype outside `src/`.

Allowed scope:

- `tools/audit-report/`
- docs explaining extraction target
- tests/fixtures that do not contain secrets or production data

Acceptance criteria:

- Reads only from an explicit SQLite file path provided by CLI argument.
- Refuses to run without an explicit input path.
- Does not read `.agent-core`, `.env`, logs, or service config by default.
- Produces `report.md` and `report.json`.
- Handles missing/empty tables gracefully.
- No Rust Kernel dependency on this tool.

Validation:

```text
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs
git diff --check
<tool-specific tests>
```

### Task D: Tool-Call Execution Design

Goal: design the smallest safe model tool-call loop for `time.now`.

Allowed scope:

- new `docs/decisions/tool-call-execution-loop.md`
- optionally `docs/current-goal.md` tracker refresh after merge

Acceptance criteria:

- Starts with `time.now` only.
- Preserves catalog -> intent -> policy -> adapter -> receipt.
- Rejects unknown/generated operations before adapter execution.
- Does not add shell, browser, workflow, memory, deployment, arbitrary HTTP, or
  unregistered tools.
- Identifies exact tests needed before implementation.

Validation:

```text
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs
git diff --check
```

## Good DeepSeek Assignments

Prefer giving DeepSeek tasks that have:

- limited files;
- explicit acceptance criteria;
- no secret access;
- no open-ended architecture;
- tests it can run locally;
- a clear PR endpoint.

Avoid giving DeepSeek tasks that require:

- deciding Kernel vs external boundaries;
- touching production services;
- interpreting real secrets or logs;
- changing approval semantics;
- introducing side-effecting tools.

## Review Checklist

Before merging a worker PR, verify:

- It touched only allowed files.
- It did not read or include secret-like content.
- It did not add new Kernel ownership for external harness behavior.
- It preserved the Rust Kernel / TS Connector boundary.
- Validation commands are reported and credible.
- `docs/current-goal.md` is updated when the task changes the next-step map.
