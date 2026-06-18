# Current Continuous Goal

This is a continuing goal for the worker agent. It is not complete after one
PR. After each merged PR, update this file with the new main-state snapshot and
choose the next smallest valuable increment.

## Standing Goal

Continue evolving Agent Core according to `docs/product-roadmap.md`.

Each iteration must:

- start from the real current `main` state;
- choose one small, mergeable increment;
- preserve the Rust Kernel / TS Connector boundary;
- avoid reading or committing `.env`, `~/.openduck`, `~/.openclaw`, logs, API
  keys, tokens, or Authorization headers;
- avoid introducing Workflow, Multi-Agent, Shell, Memory, Dynamic Hook, Plugin
  Registry, Sandbox, or Self-Evolution unless the user explicitly approves that
  phase;
- finish with a PR, validation results, residual risks, and the next candidate.

Do not mark this goal complete unless the user explicitly says to stop, pause,
or close the goal.

## Kernel Thinness Gate

Before proposing or implementing any change, answer:

1. Is this required for the kernel to produce trustworthy facts, enforce a
   safety boundary, or recover durable state?
2. Would the kernel fail to remain correct if this lived outside the process?
3. Does this change add product behavior instead of protocol/state semantics?

Default rule:

- If the answer is about Journal facts, Gateway checks, Run/Session state,
  projection repair, hash-chain integrity, or `/health`, it may belong in the
  Rust Kernel.
- If the answer is about reports, dashboards, replay runners, evaluators,
  PR automation, operator UI, workflow planning, multi-agent coordination,
  memory products, sandbox runners, or self-evolution orchestration, it belongs
  outside the Kernel.

Do not add harness code to `src/` or make the Rust Kernel depend on an audit,
replay, evaluator, or self-evolution module.

## External Harness Boundary

Long term, harness capabilities should live in separate repositories or
packages, for example:

```text
agent-core-kernel              # this repo: Rust Kernel, migrations, docs
agent-core-feishu-connector    # Feishu edge connector
agent-core-audit-tools         # read-only Journal/health report generators
agent-core-replay-eval         # replay fixtures and evaluator scripts
agent-core-evolution-harness   # branch/worktree/PR automation
```

Inside this repository, prefer documenting the protocol over adding harness
implementation. If a harness prototype must be incubated here temporarily, it
must:

- stay outside `src/`;
- be read-only against production SQLite unless explicitly approved;
- avoid becoming a Runtime dependency;
- carry a clear extraction target and removal plan;
- pass the same secret-safety checks as Kernel code.

## Update Protocol

When updating this file after an iteration:

- refresh `Current Snapshot` from the actual `main` branch;
- move resolved issues into the iteration report with PR and merge commit;
- keep unresolved issues in `Issues To Address Next`;
- add newly discovered risks with a short reason and suggested next branch;
- choose exactly one next smallest valuable increment;
- never replace this file with a generic "done" summary.

## Current Snapshot

Last reviewed main:

```text
5518b4a docs: update goal snapshot head to #116 (replay/eval design) (#117)
```

Recent work already merged:

- schema version check;
- stale dispatching lease visibility;
- release checklist;
- operating guide;
- restart recovery lifecycle test;
- outbox projection drift count;
- `RunStatus::Unknown` decision analysis;
- worker stale count in `/health`;
- operating guide update for projection drift and worker stale count;
- stale worker job re-lease regression test (PR #59);
- M5 milestones doc drift cleanup (PR #60);
- Architecture RFC domain module path fix (PR #61);
- worker failure Journal kind decision analysis (PR #62);
- `/health` rollup semantics decision analysis (PR #63);
- `RunStatus::Unknown` implemented (PR #64);
- worker failure Journal kind fixed (PR #65);
- `/health` rollup degraded on terminal-unknown + drift (PR #66);
- `/health` rollup degraded on undelivered ingress (PR #67);
- ack/clear mechanism for terminal-unknown rows decision analysis (PR #68);
- ack/clear mechanism for terminal-unknown rows implemented (PR #69);
- connector-local durability before extraction decision analysis (PR #70);
- Phase 2 Invocation Gateway Hardening scoping (PR #71);
- Phase 2 M2a operation catalog + HTTP adapter receipt status (PR #72);
- Phase 2 M2b run principal ExecutionProfile foundation (PR #73);
- Phase 2 M2b config-driven grant augmentation, closes M2b (PR #74);
- Phase 2 M2a typed adapter errors — `AdapterError` thiserror enum replaces
  string-substring `from_error` sniffing (PR #75).
- Phase 2 M2c fixed invocation policy pipeline — pure `evaluate_policy` /
  `PolicyVerdict` replaces `approve_invocation`'s inline 3-clause ladder (PR #76).
- Phase 2 M2e first read-only local adapter — `time.now` (first
  `Risk::ReadOnly`) + `TimeAdapter`, validates intent→policy→adapter→receipt
  end-to-end (PR #77).
- Phase 2 M2d durable approval state (opt-in) — `AwaitingApproval` + `ApprovalRequested/Granted/Denied` facts + `enqueue_or_pause` fork + in-process `approve_run`/`deny_run`; Phase 2 complete (PR #78).
- Phase 2 M2d follow-up — HTTP `POST /v1/approve` + `POST /v1/deny` endpoints make the resume/deny API wire-reachable (PR #79).
- Phase 2 M2d follow-up — opt-in approval expiry: a paused run older than `write_approval_ttl_secs` is expired to `Failed` with `ApprovalExpired` (PR #80).
- Phase 2 M2d follow-up — periodic approval-expiry sweep: a live timer calls `expire_stale_approvals` so a long-running server expires stalled approvals without restart (PR #81).
- Code quality: renamed `WorkerJobStatus`/`OutboxDispatchStatus` `from_str` -> `parse_opt` to clear a clippy trait-shadowing lint (PR #82).
- Code quality: clippy `needless_borrow` + `manual_clamp` fixes (PR #83).
- Code quality: removed 4 unused test imports (PR #84).
- Code quality: cleared remaining test-style clippy lints — items-after-test-module, unit-value let-binding, bool-assert-comparison (PR #85).
- docs: committed the standing continuous-goal tracker into `main` (PR #86).
- refactor: dropped the vestigial `Runtime.adapter` dead-code field + generic + dead `RecordingAdapter` test double (PR #87).
- docs(operating-guide): documented Phase 2 approval surfaces — /v1/approve + /v1/deny, awaiting_approval_count, opt-in env vars + expiry (PR #90).
- docs: synced release-checklist (Phase 1->1+2, 14->23 suites, approval boundary) + roadmap Phase 2 completion note (PR #91).
- docs(.env.example): added the missing OUTBOX_DISPATCHER_ENABLED + _POLL_MS vars; .env.example now fully in sync with config.rs (PR #93).
- docs(operating-guide): documented AGENT_CORE_EXTRA_ALLOWED_OPERATIONS (the M2b
  config-driven grant knob, missed in the earlier Phase 2 doc sync) (PR #95).

- docs(operating-guide): documented the `last_dispatch_error_category` /health field
  (present since Phase 1 PR #63 but never in the health table) (PR #97).

- feat: surface the operation catalog to the LLM as a `ToolCatalog` context block
  (Phase 2 tool-surfacing foundation, additive; `catalog_for_context()` + new
  `ContextBlockKind::ToolCatalog`) (PR #99).
- docs(operating-guide): documented all 8 LLM context blocks (incl. the new
  ToolCatalog), previously undocumented (PR #101).
- docs: updated this continuous-goal tracker after PR #101 (PR #102).

- test(connector): added execute-server payload-validation coverage (5 tests;
  connector 13->18 tests) (PR #104).
- docs: updated this continuous-goal tracker after PR #104 (PR #105).
- test(connector): covered `normalizeMessageEvent` ingress dedupe key behavior
  (PR #106).
- test(connector): covered safe-logger redaction behavior (PR #107).
- docs: External Audit Harness MVP design document (Task B) — read-only, outside
  `src/`, schema-accurate against `migrations/0001_init.sql` + `src/journal/` (PR #109).
- docs: Tool-Call Execution Loop design (Task D) — smallest safe `time.now` inline
  execution; outbox bypass, audit facts, sign-off gate (PR #112).

Open PRs at review time: none.

## Current Local Branch

On `main`, clean working tree. No in-flight feature branch. `docs/current-goal.md` is now tracked (PR #86).

## Last Iteration — PRs #109 + #112 (design documents)

- PR #109 (`0535846`): External Audit Harness MVP design (`docs/external-audit-harness.md`).
  Verified schema-accurate against the real SQLite schema + hash-chain code; all 5 Codex
  review blocking issues addressed (real table/column names, `verify_hash_chain` mirror,
  undelivered-ingress correlation logic, projection-drift join, CLI-only inputs).
- PR #112 (`8318ca5`): Tool-Call Execution Loop design
  (`docs/decisions/tool-call-execution-loop.md`). Verified all 5 review fixes addressed:
  explicit inline `time.now` path (not outbox), audit facts (InvocationProposed/Approved/
  ReceiptReceived; OutboxQueued/DispatchStarted intentionally skipped), correct file paths,
  sign-off gate. Both are doc-only; no implementation started.
- residual risks: none (design docs). The objective now moves to the Audit Harness MVP
  prototype (Task C).

## Issues To Address Next

Phase 0, Phase 1, and Phase 2 are complete. The Kernel is now a durable chat
runtime with safe invocation semantics, read-only adapter proof, opt-in durable
approval, approval HTTP endpoints, and approval expiry. Remaining work is no
longer "make the Kernel correct"; it is "make the product useful, extensible,
and observable without making the Kernel fat."

1. Documentation drift cleanup (safe doc-only PR).
   - `docs/product-roadmap.md` still contains at least one stale Phase 1 note:
     it says `RunStatus::Unknown` is undecided even though PR #64 implemented it
     and tests now cover unknown recovery.
   - Update roadmap wording to match `main` without changing scope.

2. External audit report harness MVP (recommended next product increment).
   - This is the bridge from "solid Kernel" to "late-stage self-evolution": the
     user needs a visible report of what the Agent ran before trusting replay or
     evolution.
   - Preferred destination is a separate package/repo such as
     `agent-core-audit-tools`.
   - If incubated in this repo, keep it outside `src/`, mark an extraction target,
     and make it read-only against a copied SQLite DB or explicit snapshot.
   - Output should include run/session/outbox summaries, unknown/drift health,
     hash-chain status, Git revision, and a short human-readable Markdown report.
   - It must not mutate Journal, approval state, connector state, or production DB.

3. Tool-call execution loop (requires maintainer sign-off).
   - PR #99 made the catalog visible to the model, but the model still cannot
     emit a durable tool call that becomes an `InvocationIntent`.
   - Next tool increment should start with one read-only operation (`time.now`)
     and prove unregistered/generated tools are rejected before adapter execution.
   - Do not add shell, browser, workflow, memory, deployment, or arbitrary HTTP
     tools here.

4. Connector extraction preparation (requires maintainer sign-off).
   - Feishu remains in `connectors/feishu` inside this repo.
   - Before extraction, implement connector-local execute idempotency
     persistence, symmetric to the reaction store, as scoped in
     `docs/decisions/connector-local-durability.md`.
   - Extraction target should be a separate repo/package; do not move runtime,
     gateway, journal, or policy into TypeScript.

5. Context modules and task memory (future phase, sign-off required).
   - Current context is intentionally simple and file-backed.
   - Future work may add contributor/planner/serializer contracts, but opaque
     vector memory products remain external.

6. Productization and operator packaging (future phase, sign-off required).
   - Service templates, install/upgrade flow, example connector, example
     adapter, and operator-facing docs are still needed for another developer
     to run this comfortably.

7. Known non-goals unless explicitly approved.
   - No workflow graph engine in the Kernel.
   - No multi-agent scheduler in the Kernel.
   - No shell/browser/deployment tool exposed to Feishu by default.
   - No audit/replay/evaluator/evolution module under `src/`.

8. Known code smell that needs design judgment, not a blind fix.
   - None currently blocking. The former vestigial `Runtime.adapter` concern was
     resolved by PR #87.

9. Known intentionally deferred lint.
   - `cargo clippy --all-targets -- -D warnings` still reports
     `new_without_default` on macro-generated ID newtypes.
   - Do not add `Default` blindly: a random ID in `Default::default()` is
     surprising, and an empty ID is unsafe. Prefer an explicit allow with a
     comment if this must be silenced.

## Remaining Product Gaps

What is already good enough:

- Durable Feishu/CLI chat kernel.
- Journal / hash-chain / projection recovery.
- Conservative duplicate-reply handling.
- Health and recovery surfaces.
- Operation catalog, policy pipeline, read-only adapter proof.
- Durable approval state and approval endpoints.

What is missing before it feels like a complete usable product:

- Model-emitted tool-call parsing and execution for the registered operation
  catalog. The catalog is visible as context, but not executable by the model yet.
- A first practical non-chat workflow built from safe tools and approval.
- Feishu connector extraction with connector-local execute idempotency.
- External audit report harness so the user can inspect what actually ran.
- External replay/evaluator harness for controlled self-evolution.
- Packaging/service/runbook polish for repeatable installation and upgrades.

Completion estimates assume one focused coding agent, quick maintainer
decisions, and no major redesign:

| Target | Rough Estimate | Notes |
|---|---:|---|
| Stable personal dogfooding chat/runtime | Done | Current `main` is already here. |
| Tool catalog visible to the model | Done | PR #99 adds `ToolCatalog` context. |
| Model can execute one safe registered tool | 3-7 days | Start with `time.now`, no arbitrary tools. |
| Broader safe-tool loop with approval | 1-2 weeks | More schemas, tests, and user-visible failure states. |
| Feishu connector ready to extract | 1 week | Execute idempotency persistence + extraction checklist/tests. |
| External audit report harness MVP | 3-5 days | Must live outside Kernel; read-only SQLite/health report. |
| Replay/eval/self-evolution rehearsal | 2-3 weeks | External harness, selected fixtures, PR-only promotion. |
| Broader productized system | 6-10 weeks | Packaging, examples, connector split, docs, upgrade path. |

## Next Recommended Increment

Main is clean and `pnpm check` passes. The project is no longer in early Kernel
construction. It is at the transition from Phase 2 to Phase 3/6 preparation:

Read `docs/agent-dispatch.md` before delegating work to another agent. It
defines the branch flow, task-packet format, hard safety rules, and current
parallelizable task shapes.

1. **Recommended next increment: external audit report harness MVP.**
   - Branch/package: `feat/external-audit-report-mvp` if incubated here, or a
     separate `agent-core-audit-tools` repo/package.
   - Keep all implementation outside `src/`.
   - Read from an explicit copied SQLite DB/snapshot plus optional `/health`
     JSON. Do not read secret files or production logs.
   - Produce `report.md` and `report.json` with: Git revision, schema version,
     hash-chain verification result, recent runs, unknown dispatches, projection
     drift, undelivered ingress, approval waits/expiries, and duplicate-reply
     safety notes.
   - This unlocks user-visible acceptance of Agent-run behavior.

2. **After audit MVP: replay/eval rehearsal.**
   - Define replay fixture format from audited runs.
   - Run candidate branch/worktree against selected fixtures.
   - Produce `score.json` and `report.md`.
   - Promote only through PR; never mutate production runtime in place.

3. **Parallel but lower priority: model tool-call execution.**
   - Start with `time.now` only.
   - Preserve catalog -> intent -> policy -> adapter -> receipt.
   - Reject unknown/generated operations before adapter execution.

Do not start self-evolution orchestration before the audit report MVP exists.
Without a visible audit report, the user cannot verify what changed or why.

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
