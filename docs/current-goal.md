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
7695335 refactor: clear remaining test-style clippy lints (#85)
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

Open PRs at review time: none.

## Current Local Branch

On `main`, clean working tree (only this untracked goal doc is present
locally). No in-flight feature branch.

## Last Iteration — PR #85

- branch: `refactor/clippy-test-style` (squash-merged, branch deleted);
- merge commit: `7695335`;
- approval: behavior-preserving clippy fixes, no sign-off.
- files changed: `src/journal/unknown.rs` (moved `parse_time` above `mod tests`),
  `src/server/delivery_tests.rs` (dropped unit-value binding + meaningless
  assert), `tests/m5_parse_kind.rs` (`assert_eq!(x,false)` -> `assert!(!x)`).
- implementation: cleared 3 test-style clippy lints. All behavior-preserving.
- validation: cargo build/test green, 3 lints cleared, pnpm check ok, structure
  + secret-leak + validation_layout passed.
- resolved issue: all *safe* clippy debt is now cleared. The 6 remaining lints
  are `Default`-impl suggestions on macro-generated domain id-types
  (`RunId`/`AgentId`/...) — deliberately left, since a `Default` impl there
  would risk a misleading empty id masking bugs (a real correctness hazard,
  not cosmetic).
- residual risks: none from this PR. The `Default` lints are correctly deferred.
## Issues To Address Next

Phase 1 is complete; Phase 2 is scoped (PR #71); Phase 3 pre-scoping is done
(PR #70). Phase 2 M2a (operation catalog + HTTP adapter receipt status +
typed errors) is **done** (PRs #72, #75); Phase 2 M2b (run principal execution
profile: foundation + config driven) is **fully done** (PRs #73, #74); Phase 2
M2c (fixed policy pipeline) is **done** (PR #76); Phase 2 M2e (first read-only
local adapter) is **done** (PR #77); Phase 2 M2d (durable approval state) is
**done** (PR #78). **Phase 2 is complete.** The remaining work is a menu of
phase increments, each gated on individual maintainer sign-off:

1. Phase 2 Invocation Gateway Hardening — increments M2a–M2e scoped in
   `docs/decisions/phase2-invocation-gateway-scoping.md`.
   - **M2a** — operation catalog + HTTP adapter receipt status + typed errors
     — **done** (PR #72, #75).
   - **M2b** — run principal execution profile — **done** (PR #73 foundation +
     PR #74 config-driven).
   - **M2c** — fixed policy pipeline — **done** (PR #76).
   - **M2d** — durable approval state (opt-in; only `risk: Write` operations
     pause) — **done** (PR #78).
   - **M2e** — first read-only local adapter — **done** (PR #77).
   - Phase 2 is **complete** (all of M2a–M2e merged, PRs #72–#78).

2. Phase 3 connector extraction — durability scoped in
   `docs/decisions/connector-local-durability.md`.
   - Plan B (connector-local execute-idempotency persistence, symmetric to the
     reaction store) is a **mandatory pre-extraction checklist item**.
   - Extraction itself is gated on explicit approval.

3. Keep audit/replay/self-evolution out of the Kernel.
   - Audit reports, replay runners, evaluators, and self-evolution orchestration
     should be external harness/plugin repos, not new Rust Kernel modules.
   - If a future PR proposes `src/audit`, `src/replay`, `src/evaluator`, or
     `src/evolution`, reject it unless the user explicitly approved moving that
     boundary into the Kernel.

4. Note on a tempting-but-wrong "non-gated" candidate:
   - `src/server/delivery.rs` has a **local `error_category`** that still does
     string-substring sniffing (`contains("timeout")` etc.). Investigated as a
     possible behavior-preserving dedup of PR #75, but **rejected**: its
     category set (`timeout` / `connector_execute_failed` /
     `target_session_mismatch` / `runtime_failed`) is **not** the
     `DispatchErrorCategory` enum — `runtime_failed` and `target_session_mismatch`
     have no DispatchErrorCategory equivalent, and these strings are persisted
     into Journal `RunFailed` events and worker-job failure rows
     (`tests/m1_worker_failure_journal_kind.rs` asserts `runtime_failed`;
     `tests/m5_outbox_recovery.rs` asserts `timeout`). Routing it through
     `DispatchErrorCategory::from_error` would **change persisted values and
     break tests** — a semantic change, not a refactor. Left as-is; any change
     here needs its own sign-off and a deliberate category mapping.

4. Discovered code smell (needs design judgment, not a blind fix):
   - `src/runtime/mod.rs:17` — `Runtime` stores `adapter: A` behind
     `#[allow(dead_code)]`; the field is never read (dispatch goes through the
     outbox dispatcher, not Runtime). Likely vestigial from before the
     outbox-dispatch split. Removing it changes the public `Runtime::new` API
     + all callers, and raises a design question (was the adapter *meant* to be
     used inline?). Branch `refactor/runtime-drop-vestigial-adapter` — needs
     sign-off (API change), and is not obviously safe to auto-merge.

## Next Recommended Increment

Phase 1 complete; **Phase 2 complete** incl. all M2d follow-ups (M2a/M2b/M2c/
M2d/M2e + HTTP endpoint + approval expiry + periodic sweep, PRs #72–#81);
Phase 3 pre-scoped.

The loop is at a **sign-off gate**. Remaining options, each gated on maintainer
approval (each changes protocol/state semantics or contradicts a recorded
decision):

- **Approve Phase 3 plan B** → branch `feat/connector-execute-idempotency`
  (connector-local execute-idempotency persistence — the mandatory pre-extraction
  checklist item; connector-side TS, not kernel `src/`). **Note**:
  `docs/decisions/connector-local-durability.md` explicitly scopes Plan B to
  ship *with* the extraction PR, not before — implementing it now would
  contradict the recorded decision.
- **Approve surfacing read-only/Write tools to the LLM** (context/tool-schema
  increment) → branch `feat/tool-surfacing`.
- **Say STOP** to end the loop.

Absent direction, no remaining increment is both safe and non-trivial without
new sign-off.

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
