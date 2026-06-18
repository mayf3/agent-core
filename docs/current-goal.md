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

On `main` after PR #134. No open PRs at the time of this update.

Phase 0/1/2 are dogfood-ready: durable Feishu/CLI chat kernel, Journal/hash-chain/projection recovery, conservative duplicate-reply handling, health + recovery surfaces, operation catalog + policy pipeline + read-only adapter proof, durable approval state + approval endpoints, `ToolCatalog` visible to the model, and one model-emitted read-only tool (`time.now`).

External harness state:

- **`tools/audit-report`** — read-only audit MVP, hardened. `projection_drift`
  now mirrors `outbox_projection_drift_count` (terminal Journal fact vs
  projection status, not a naive `dispatching` count). `undelivered_ingress`
  mirrors `src/journal/recovery.rs` exactly (delivered set = correlation_ids of
  SessionReady/RunStarted/RunCompleted/RunFailed; an IngressAccepted is
  undelivered only when `payload.event_id` is present and not in that set).
  7 tests. (PRs #114, #131.)
- **`tools/replay-eval`** — replay/eval MVP + synthetic fixture pack +
  scorer hardening. Drives a candidate build on ephemeral port/DB/worktree,
  scores vs baseline, writes `score.json` + `report.md`. Fixtures cover
  `forbidden_operations` / `policy_verdict` / `reply_contains_any`. Hard-fail
  details are always structured `ExpectationResult` objects (comma-expression
  leak fixed; per-branch + cross-cutting regressions). 50 tests. (PRs #125,
  #128, #130, #132, #134.)

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

## Issues To Address Next

1. **External harness productization** (current sprint, in progress).
   - Replay-eval batch/suite mode (multiple fixtures in one run, one aggregated
     `score.json`/`report.md`).
   - Acceptance runbook (`docs/harness-acceptance-runbook.md`): how to run each
     harness, how to read the reports, the red-lines that block merge
     (hash-chain faulty, unacked unknown dispatch, projection drift, undelivered
     ingress, duplicate-reply collision, replay hardFail/regress).

2. **Connector extraction preparation.**
   - Feishu remains in `connectors/feishu` inside this repo.
   - Before extraction, implement connector-local execute idempotency
     persistence, symmetric to the reaction store, as scoped in
     `docs/decisions/connector-local-durability.md` (Plan B ships *with*
     extraction).
   - Extraction target should be a separate repo/package; do not move runtime,
     gateway, journal, or policy into TypeScript.

3. **First safe practical tool / broader tool loop.**
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

- Replay-eval batch/suite mode + acceptance runbook for user-visible gates.
- Feishu connector extraction with connector-local execute idempotency.
- A first practical non-chat workflow built from safe tools and approval.
- Packaging/service/runbook polish for repeatable installation and upgrades.

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

The old audit/tool-call goal is retired. Give the worker agent this next goal:

```text
Read docs/current-goal.md, docs/agent-dispatch.md, docs/replay-eval-harness.md,
and tools/audit-report/README.md.

Goal: implement the Replay/Eval Harness MVP outside src/. It should evaluate one
candidate branch or worktree against explicit fixtures or copied snapshots,
produce score.json and report.md, and never touch live services, production
SQLite, secret files, .env, ~/.openduck, or ~/.openclaw. Keep promotion manual
through PR only. Do not implement self-evolution orchestration, workflow engine,
multi-agent scheduling, shell/browser/deploy tools, or Kernel changes unless a
bug is proven and split into a separate PR.
```

Acceptance for that goal:

- `pnpm check` passes.
- A synthetic fixture can be replayed deterministically.
- `score.json` includes candidate git revision, fixture results, pass/fail
  counts, and a machine-readable overall score.
- `report.md` summarizes baseline/candidate comparison, failures, and residual
  risks in a form the user can inspect.
- No implementation is added under `src/`.

Do not start self-evolution orchestration before replay/eval can produce a
visible report. Without a report, the user cannot verify what changed or why.

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
