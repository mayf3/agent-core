# External Self-Evolution Rehearsal Harness — Design

## 1. Purpose

The evolution harness is the third external capability in Agent Core's
late-stage pipeline (audit → replay/eval → **self-evolution rehearsal**). It
strings an already-built kernel + replay/eval + audit into an experienceable
loop:

> goal → candidate branch/worktree → delegate to a worker agent → replay/eval
> suite → optional audit-report against a copied snapshot → write an
> evolution report → open a PR → **human/Codex review → manual merge only**.

It is the "rehearsal" half of controlled self-evolution: the harness can
*prepare* and *evaluate* a candidate, but a human (or separately-reviewed
automation) *merges* it after reading the report. It is explicitly **not**
auto-merge, **not** a workflow engine, and **not** multi-agent orchestration.

This is a **design document only** — no implementation in this PR.

## 2. Boundary & Ownership

- **External to the Rust Kernel.** The harness lives in
  `tools/evolution-harness/` (incubation) or a separate
  `agent-core-evolution-harness` package (extraction target). It **must never**
  live under `src/`, and the Kernel must never depend on it.
- The harness is a **client** of the Kernel (over HTTP) and an **orchestrator**
  of the existing `tools/audit-report` + `tools/replay-eval`. It does not link
  Kernel code and does not mutate Journal/approval/connector state.
- It conforms to the hard rules in `docs/agent-dispatch.md`: no `.env` /
  `~/.openduck` / `~/.openclaw` / logs / API keys / tokens / Authorization /
  live production DB; no service stop/restart.
- **No Kernel `src/` writes** unless the goal the harness is executing
  explicitly targets the Kernel AND that change is carried in a separately-
  reviewed PR. The harness itself never edits `src/`.

## 3. Inputs

| Input | Required | Description |
|---|---|---|
| `--goal <file>` | Yes | A goal file (e.g. `docs/current-goal.md`) describing what the candidate should achieve. The harness reads it to seed the plan; it does not execute arbitrary code from it. |
| `--candidate <git ref>` | Yes | The candidate branch to evaluate. Must resolve to a git ref. |
| `--base <git ref>` | No | The baseline ref (default `main`). |
| `--fixtures-dir <dir>` | No | Fixtures for the replay/eval suite (passed through to `tools/replay-eval`). |
| `--audit-db <path>` | No | A **copied** SQLite snapshot to run `tools/audit-report` against. Never the live DB. |
| `--out-dir <dir>` | No | Where to write run artifacts. Default: `tools/evolution-harness/runs/<run-id>/`. |
| `--dry-run` | No | Default `true` for the skeleton: produce the plan + report without spawning a worker agent or mutating any repo state. |

The harness **must refuse** to run if any input path resolves to `.env`,
`.agent-core`, `~/.openduck`, `~/.openclaw`, logs, or a production DB, or if a
git ref cannot be resolved (rejecting unsafe/ambiguous refs).

## 4. MVP Flow

```
 1. load + sanitize the goal file
 2. validate candidate + base git refs (reject unsafe refs)
 3. create run directory: tools/evolution-harness/runs/<run-id>/
 4. emit plan.json (goal summary + candidate/base refs + planned steps)
 5. (non-dry-run only) delegate to a worker agent on the candidate branch:
      - the worker implements the goal in a temp worktree
      - the harness does NOT auto-commit/merge/push
 6. (optional) run replay-eval suite (--fixtures-dir) → score.json
 7. (optional) run audit-report against a --audit-db copy → report.json
 8. emit evolution-report.md (links plan.json / score.json / audit report)
 9. (non-dry-run only) optionally open a PR; NEVER auto-merge
10. handoff: a human/Codex reviews the PR + reports; manual merge only
```

`dry-run` (default for the skeleton) stops at step 4 + a synthetic report — it
never spawns a worker agent, never calls `git push`/`merge`, never opens a PR.
This makes the skeleton safe to run by anyone at any time.

## 5. Run Directory

All artifacts for one run land under
`tools/evolution-harness/runs/<run-id>/`, where `<run-id>` is a timestamp +
short random suffix (e.g. `20260619-1030-a1b2`). Contents:

- `plan.json` — the goal summary + candidate/base refs + planned steps.
- `score.json` — the replay/eval result (copied from `tools/replay-eval`, if
  run).
- `audit-report.json` — the audit result (copied from `tools/audit-report`, if
  run).
- `evolution-report.md` — the human-readable summary linking the above.
- `manifest.json` — the exact commands run, exit codes, timestamps (provenance).

The run directory is the single place a reviewer inspects to decide whether to
merge the candidate.

## 6. Red-lines

The harness **must not**:

- Read `.env`, `~/.openduck`, `~/.openclaw`, logs, API keys, tokens,
  Authorization, or the live production DB.
- Stop or restart any service.
- Auto-merge, auto-push to `main`, or open a PR without explicit `--pr` (and
  even then, merge stays manual).
- Write to Kernel `src/` unless the goal explicitly targets the Kernel and the
  change is in a separately-reviewed PR.
- Implement a workflow engine, multi-agent scheduler, shell/browser/deploy
  tool, or any in-process "evolution loop" that mutates production.
- Promote a candidate whose replay/eval verdict is `regress` or that has a
  hard-fail.

## 7. Relationship to existing harnesses

- **`tools/audit-report`** — read-only audit; the evolution harness *calls* it
  with a copied `--audit-db`, never feeds it the live DB.
- **`tools/replay-eval`** — drives + scores a candidate; the evolution harness
  *calls* it with `--fixtures-dir` and copies its `score.json` into the run
  directory.
- **`docs/harness-acceptance-runbook.md`** — the red-lines there (hash-chain
  faulty, unacked unknown dispatch, projection drift, undelivered ingress,
  duplicate-reply collision, replay hardFail/regress) are the merge blockers
  the evolution harness surfaces in its report.

The evolution harness **composes** these; it does not re-implement them.

## 8. Safety & Non-Goals

- **Not autonomous.** Promotion requires a human (or separately-reviewed
  automation) to merge after reading the report. The harness only prepares +
  evaluates + reports.
- **Not a model trainer.** The harness scores + reports; it does not update
  weights or edit model files.
- **Not a Kernel module.** All logic is in `tools/` or the external package.
- The MVP skeleton **does not** spawn a real worker agent — it produces a plan
  + report in dry-run. Wiring a real worker (and `gh pr create`) is a later,
  separately-reviewed increment.

## 9. Implementation Plan (post-design)

1. **CLI skeleton** (`tools/evolution-harness/cli.ts`) — goal load, ref safety,
   run-dir creation, `plan.json` + `evolution-report.md` in dry-run. No worker
   agent, no push/merge. (PR3.)
2. **Composition** — optionally call `tools/replay-eval` + `tools/audit-report`
   and copy their outputs into the run dir.
3. **Tests** — forbidden-path rejection, unsafe-ref rejection, dry-run
   produces plan/report, never invokes `git push`/`merge`.
4. **(Later)** real worker-agent delegation + `--pr` (still manual merge).

Each step is its own small PR; this design merges first.

## 10. Validation (this PR, doc-only)

```text
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs
git diff --check
```
