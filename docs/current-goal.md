# Current Continuous Goal

This file is the standing goal for worker agents. It is not complete after one
PR. After each merged PR, update this file with the real `main` snapshot, the
merged work, residual risks, and exactly one next smallest valuable increment.

Do not mark this goal complete unless the user explicitly says to stop, pause,
or close it.

## Standing Goal

Continue evolving Agent Core according to `docs/product-roadmap.md`, while
keeping the Kernel thin.

Each iteration must:

- start from the actual current `main`;
- choose one small, mergeable increment;
- preserve the Rust Kernel / TS Connector boundary;
- avoid reading or committing `.env`, `~/.openduck`, `~/.openclaw`, logs, API
  keys, tokens, Authorization headers, or production databases;
- avoid stopping or restarting user services unless explicitly asked;
- avoid adding Workflow, Multi-Agent, Shell, Memory, Dynamic Hook, Plugin
  Registry, Sandbox, or Self-Evolution unless the user explicitly approves that
  phase;
- finish with a PR, validation results, residual risks, and the next candidate.

## Codex Delegation Mode

Codex should conserve tokens by default.

Codex's default role is:

- clarify architecture and acceptance criteria;
- write or update task packets in docs;
- review PRs after worker agents finish;
- inspect boundary, safety, state-machine, duplicate-reply, and secret risks;
- decide whether the next worker task is ready.

Codex should not directly implement code changes unless the user explicitly
asks Codex to do the implementation. When work is needed, Codex should record
the task in this file or in `docs/agent-dispatch.md`, then delegate to GLM or
DeepSeek.

Model allocation:

- GLM: use sparingly for boundary-sensitive starts, design-to-implementation
  scoping, and first-pass skeletons.
- DeepSeek: use for low-risk follow-up work such as fixtures, tests, docs,
  validation scripts, and narrow external-harness improvements.
- Codex: review batched PRs, identify blockers, and update the next goal.

## Kernel Thinness Gate

Before proposing or implementing any change, answer:

1. Is this required for trustworthy Journal facts, Gateway enforcement, durable
   Run/Session state, projection repair, hash-chain integrity, or `/health`?
2. Would correctness break if this lived outside the Kernel process?
3. Does this add product behavior instead of protocol/state semantics?

Default rule:

- Kernel work may live in `src/` only when it is about Journal facts, Gateway
  checks, Run/Session state, projection repair, hash-chain integrity, recovery,
  or `/health`.
- Reports, dashboards, replay runners, evaluators, PR automation, operator UI,
  workflow planning, multi-agent coordination, memory products, sandbox runners,
  and self-evolution orchestration belong outside the Kernel.

Do not add harness code to `src/` or make the Rust Kernel depend on audit,
replay, evaluator, or self-evolution modules.

## External Harness Boundary

Long term, harness capabilities should live in separate repositories or
packages:

```text
agent-core-kernel              # this repo: Rust Kernel, migrations, docs
agent-core-feishu-connector    # Feishu edge connector
agent-core-audit-tools         # read-only Journal/health report generators
agent-core-replay-eval         # replay fixtures and evaluator scripts
agent-core-evolution-harness   # branch/worktree/PR automation
```

If a harness prototype is incubated here temporarily, it must:

- stay outside `src/`;
- be read-only against production SQLite unless explicitly approved;
- avoid becoming a Runtime dependency;
- carry a clear extraction target and removal plan;
- pass the same secret-safety checks as Kernel code.

## Current Snapshot

Functional baseline reviewed:

```text
0b1338d Merge pull request #121 from mayf3/fix/tool-call-single-execution
```

Doc-only refreshes may follow this baseline without changing the functional
state described below.

- feat(tools): Replay/Eval Harness MVP (`tools/replay-eval/`) — drives a candidate build
  against a fixture on ephemeral port+DB+worktree, scores vs baseline, writes
  score.json + report.md. 14 tests (fixture validation + scoring). External, no new
  dependency, PR-only promotion (PR #125).

- test(replay-eval): synthetic fixture pack (forbidden_operations, policy_verdict,
  reply_contains) — expands fixture coverage for the scorer (PR #130).
- feat(audit-report): aligned projection_drift + undelivered_ingress with Rust Journal
  semantics. undeliveredIngress now mirrors recovery.rs exactly — delivered set is the
  correlation_ids of SessionReady/RunStarted/RunCompleted/RunFailed; an IngressAccepted is
  undelivered only when payload.event_id is present and not in that set (not the old
  correlation_id match). Regression test added (PR #131).
- fix(replay-eval): scorer hard-fail branches used a comma expression that pushed a
  boolean into details instead of the ExpectationResult object; rewritten as if/else +
  3 regression tests (PR #132).

Open PRs at review time: none.

High-signal state:

- Phase 0 and Phase 1 are complete enough for dogfooding: durable ingress,
  Journal, worker/outbox projections, dispatcher loop, recovery, health, and
  conservative duplicate-reply handling are implemented.
- Phase 2 is complete enough for a first safe tool loop: operation catalog,
  run-principal grants, fixed policy pipeline, typed adapter errors, opt-in
  write approval, approval endpoints, expiry sweep, ToolCatalog context, and
  `time.now` read-only adapter are merged.
- External harness work has started outside the Kernel: `tools/audit-report`
  exists as a read-only audit report MVP (PR #114), and
  `docs/replay-eval-harness.md` defines the replay/eval harness design (PR #116).
- Model-emitted `time.now` execution MVP is merged (PR #118), and PR #121 fixed
  the inline execution regression: a single inline tool call executes exactly
  once, and the Runtime pins the target `session_id` before policy evaluation.

## Current State

On `main` at `ec66fc7` (after PR #153). Open PR #154 (feat/session-recall-loop) — not merged, candidate only.

Phase 3 (Connector Extraction Readiness) is complete: connector-local execute
idempotency persists across restart (PR #139), and the extraction checklist +
connector README landed (PR #140). The **External Self-Evolution Rehearsal
harness** now has a **real evaluation-only loop**: a hardened, no-shell CLI
that pins candidate/base commits, composes `tools/replay-eval` (+ optional
`tools/audit-report` against a copied snapshot), writes an evidence package,
and derives a `pass`/`blocked` decision from the red-lines (PRs #142–#146).
Merge is always manual.

Phase 0/1/2 are dogfood-ready: durable Feishu/CLI chat kernel, Journal/hash-chain/projection recovery, conservative duplicate-reply handling, health + recovery surfaces, operation catalog + policy pipeline + read-only adapter proof, durable approval state + approval endpoints, `ToolCatalog` visible to the model, and a bounded read-only tool-recall loop (open PR #154) exposing three tools (`time.now`, `session.recall_recent`, `system.status`) with strict schemas, real provider support, provider ID hashing, and sanitized error boundaries.

External harness state (productization sprint complete, PRs #135/#136/#137):

- **`tools/audit-report`** — read-only audit MVP, hardened. `projection_drift`
  now mirrors `outbox_projection_drift_count` (terminal Journal fact vs
  projection status, not a naive `dispatching` count). `undelivered_ingress`
  mirrors `src/journal/recovery.rs` exactly (delivered set = correlation_ids of
  SessionReady/RunStarted/RunCompleted/RunFailed; an IngressAccepted is
  undelivered only when `payload.event_id` is present and not in that set).
  7 tests. (PRs #114, #131.)
- **`tools/replay-eval`** — replay/eval MVP + synthetic fixture pack +
  scorer hardening + batch/suite mode (`--fixtures-dir`). Drives a candidate
  build on ephemeral port/DB/worktree, scores vs baseline, writes one aggregated
  `score.json` + `report.md`. Fixtures cover `forbidden_operations` /
  `policy_verdict` / `reply_contains_any`. Hard-fail details are always structured
  `ExpectationResult` objects (comma-expression leak fixed; per-branch +
  cross-cutting regressions). 54 tests. (PRs #125, #128, #130, #132, #134, #136.)
- **`docs/harness-acceptance-runbook.md`** — how to run each harness, how to
  read the reports, the red-lines that block merge, plus a manual acceptance
  checklist (PR #137).

Validation:

```text
pnpm check
node --test --experimental-strip-types tools/replay-eval/test/*.test.ts
node --test --experimental-strip-types tools/audit-report/test/*.test.ts
```

Result: replay-eval 50/50, audit-report 7/7, structure + secret-scan + diff --check clean.

## Completed Recently

- PR #114: external audit report harness MVP under `tools/audit-report`, outside `src/`.
- PR #116: replay/eval harness design document.
- PR #118: inline `time.now` model tool-call execution MVP.
- PR #121: inline tool-call regression fix.
- PR #125: replay/eval harness MVP (`tools/replay-eval`).
- PR #128: replay-eval minimal-env hardening (no `process.env` secret leak).
- PR #130: replay-eval synthetic fixture pack.
- PR #131: audit-report `projection_drift` + `undelivered_ingress` Rust-semantics alignment.
- PR #132/#134: replay-eval hard-fail details structured-object fix + regression coverage.
- PR #154 (open, not merged): bounded read-only tool-recall loop with real provider support, strict schemas,
  argument validation, sanitized error boundaries, provider ID hashing, and exact bounded-loop tests.

## Issues To Address Next

1. **External harness productization** (sprint complete).
   - ✅ Replay-eval batch/suite mode (`--fixtures-dir`, one aggregated report).
   - ✅ Acceptance runbook (`docs/harness-acceptance-runbook.md`): how to run
     each harness, how to read the reports, the red-lines that block merge
     (hash-chain faulty, unacked unknown dispatch, projection drift, undelivered
     ingress, duplicate-reply collision, replay hardFail/regress), plus a
     manual acceptance checklist.

2. **Connector extraction preparation** (next recommended goal).
   - Feishu remains in `connectors/feishu` inside this repo.
   - Before extraction, implement connector-local execute idempotency
     persistence, symmetric to the reaction store, as scoped in
     `docs/decisions/connector-local-durability.md` (Plan B ships *with*
     extraction).
   - Extraction target should be a separate repo/package; do not move runtime,
     gateway, journal, or policy into TypeScript.

3. **First safe practical tool / broader tool loop.** Implemented in PR #154.
   - `time.now` proves the minimum read-only path.
   - More tools require strict schemas, session scoping, audit facts, and
     approval for write/external effects.
   - Do not add arbitrary HTTP, shell, browser, deployment, or memory tools by
     default.

## Remaining Product Gaps

Already good enough:

- Durable Feishu/CLI chat kernel.
- Journal / hash-chain / projection recovery.
- Conservative duplicate-reply handling.
- Health and recovery surfaces.
- Operation catalog, policy pipeline, read-only adapter proof.
- Durable approval state and approval endpoints.
- ToolCatalog visible to the model.
- One model-emitted read-only tool (`time.now`).
- Read-only audit report (hardened) outside `src/`.
- Replay/eval harness MVP + fixtures + scorer hardening outside `src/`.

Still missing before it feels like a complete product:

- Feishu connector extraction with connector-local execute idempotency.
- A first practical non-chat workflow built from safe tools and approval.
- Packaging/service polish for repeatable installation and upgrades.

Rough estimates, assuming one focused coding agent and quick decisions:

| Target | Estimate | Notes |
|---|---:|---|
| Stable personal dogfooding chat/runtime | Done | Current main is already here. |
| Tool catalog visible to model | Done | PR #99. |
| One safe model-emitted tool | Done | `time.now`, no arbitrary tools; PR #121 hardens execution. |
| Audit report harness MVP | Done, hardening remains | PR #114. |
| Replay/eval harness MVP | 1-2 weeks | External harness, selected fixtures. |
| Feishu connector ready to extract | 1 week | Execute idempotency + extraction checklist. |
| Broader safe-tool loop with approval | 1-2 weeks | More schemas and user-visible failures. |
| Self-evolution rehearsal | 2-3 weeks after replay/eval | Branch/worktree + PR-only promotion. |
| Broader productized system | 6-10 weeks | Packaging, examples, connector split, docs. |

## Next Recommended Goal

The External Self-Evolution Rehearsal harness now has a **real evaluation-only
loop** on `main` (PRs #142–#146): given a candidate git ref, it pins the
candidate/base commits, composes `tools/replay-eval` (+ optional
`tools/audit-report` against a copied snapshot), writes a full evidence package
(plan.json, manifest.json, evolution-report.md, replay score.json/report.md,
optional audit report.json/report.md), and derives an explicit
`pass`/`blocked` decision from the red-lines. **Merge is always manual.**

The next goal is **worker-agent/worktree delegation** — but this Goal does
**not** implement it. It is the next phase. Give the worker agent this next
goal:

```text
Goal: Worker-agent delegation for the External Self-Evolution Harness (design
+ task-pack only this phase; no auto-dispatch/auto-code-change/auto-PR). The
harness currently evaluates a candidate the user already provides; the next
phase lets it prepare that candidate by spawning a worker agent on a temp
worktree. NEVER auto-commit/merge/push; a PR is opened only behind an explicit
--pr flag; merge stays manual.

PR1 — design doc for worker-agent delegation (temp worktree, goal handoff,
safety: no src/ writes unless the goal targets the Kernel + separate review,
no secret/prod-DB reads, no service control).
PR2 — (later, separately approved) implementation + tests using an injectable
agent runner (no real network/prod).

Boundaries: no Rust Kernel src/; no auto-promotion; no workflow/multi-agent/
shell/browser/deploy; no secret/log/prod-DB reads; manual merge only; one PR
per topic, ≤3 open PRs.
```

Acceptance for that goal:

- The design doc defines the temp-worktree + goal-handoff + safety model.
- No implementation is added under `src/`. Merge is always manual.

What is **already done** (this phase, PRs #145/#146): the harness can evaluate
a real candidate ref end-to-end and produce a `pass`/`blocked` decision with a
full evidence package — the "given a candidate branch → automatic evaluation →
explicit pass/block" loop is experienceable now.

## Validation Rule

For doc-only changes:

```text
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs
git diff --check
```

For Rust or TypeScript behavior changes:

```text
pnpm check
cargo build
```

For M5/Phase 1 invariant-sensitive changes, also run:

```text
python3 docs/m5-outbox-stabilization/validation_layout.py
```

## Handoff Format

After each iteration, report:

- branch;
- PR;
- merge commit;
- validation commands and results;
- files changed;
- residual risks;
- next recommended increment.
