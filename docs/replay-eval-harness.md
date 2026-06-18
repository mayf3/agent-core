# Replay/Eval Harness — Design

## 1. Purpose

The Replay/Eval harness is the second external capability in Agent Core's
late-stage pipeline (audit → replay/eval → controlled self-evolution). It
replays recorded conversations through a **candidate** build of the Kernel and
scores the result against the recorded **baseline**, so a change to the Kernel
or model can be evaluated *before* it is merged. It is the gate that makes
"controlled self-evolution" safe: candidate code only reaches `main` via a PR
that carries its own score report.

This is a **design document only** — no implementation in this PR.

## 2. Boundary & Ownership

- **External to the Rust Kernel.** The harness lives in `tools/replay-eval/`
  (incubation) or a separate `agent-core-replay-eval` package (extraction
  target). It **must never** live under `src/` and the Kernel must never
  depend on it.
- The harness is a **client** of the Kernel over its existing HTTP surface
  (`/v1/ingress`, `/v1/approve`, `/v1/deny`, `/health`). It does **not** link
  Kernel code, does **not** open the Kernel's SQLite DB for writes, and does
  **not** mutate Journal/approval/connector state. The only writes are its own
  output files (`score.json`, `report.md`, worktree branches).
- It conforms to the hard rules in `docs/agent-dispatch.md`: no `.env` /
  `.openduck` / `.openclaw` / logs / API keys / tokens / Authorization / live
  production DB; no service stop/restart of production.

## 3. Inputs

A replay run takes:

| Input | Required | Format | Description |
|---|---|---|---|
| `--fixture` | Yes | File path | A replay fixture (see §4) — a recorded conversation + expectations. |
| `--candidate` | Yes | Git ref | The candidate Kernel/model build to evaluate. The harness checks this out in a temporary worktree, builds it, and runs it. |
| `--baseline` | No | Git ref | The baseline build to compare against. Defaults to `main`. The harness runs the same fixtures against the baseline and diffs. |
| `--model-config` | No | File path | Model provider config for the run (base URL, model name). Must NOT contain live API keys — the harness reads keys from the environment of the operator, never from a committed file. |
| `--out-dir` | No | Dir path | Where to write `score.json` + `report.md`. Default: cwd. |

The harness **must refuse** to run if:

- `--fixture` is missing, not a regular file, or fails schema validation.
- `--candidate` is missing or does not resolve to a Git ref.
- The candidate worktree cannot be built (`cargo build` fails).
- Any input path resolves to `.env`, `.agent-core`, logs, or a production DB.

## 4. Fixture Format

A fixture is a single JSON file representing one reproducible conversation. It
is derived from an **audited** run (via the External Audit Harness,
`docs/external-audit-harness.md`) or authored by hand for a regression case.

```jsonc
{
  "schema_version": 1,
  "fixture_id": "feishu-greeting-001",
  "description": "User greets the agent; agent replies politely.",
  "source": {
    "kind": "audited",              // "audited" | "authored"
    "run_id": "run_abc123",          // present when kind=audited
    "git_revision": "abc123..."      // the Kernel rev that produced the original
  },
  "setup": {
    "agent_id": "main",
    "channel": "feishu",
    "session_id": "sess_fixture_1"
  },
  "turns": [
    {
      "role": "user",
      "external_event_id": "msg_in_1",
      "text": "你好"
    }
  ],
  "expectations": {
    // Soft assertions — each is scored, not hard-failed (see §6).
    "reply_operations": ["feishu.send_message"],   // the operations the agent should emit
    "reply_contains_any": ["你好", "hi", "hello"],  // the reply text should contain one of these
    "no_duplicate_reply": true,                     // exactly one dispatch per ingress
    "max_latency_ms": 5000,                         // optional perf expectation
    "policy_verdict": "allow",                      // the intent must be allowed (not denied)
    "forbidden_operations": ["shell.exec"]          // must never appear
  }
}
```

Design notes:

- Fixtures are **deterministic seeds**, not golden outputs. The expectations
  are deliberately soft (`reply_contains_any`) so that a better-phrased reply
  is not penalized as a regression. Exact-string matching is an anti-pattern
  for LLM behavior and is forbidden by default.
- The `turns` array uses the Kernel's ingress envelope shape
  (`external_event_id`, `source`, `payload.text`) so the harness can replay
  them verbatim over `/v1/ingress`.
- A fixture may declare `expectations` as `{}` — then it is a **smoke** replay
  (does the candidate not crash / not duplicate-reply), useful for
  regression coverage even without semantic expectations.

## 5. Candidate Promotion Model

Candidate code reaches `main` **only through a PR**. The harness enforces this:

1. The harness checks out `--candidate` into a **temporary git worktree**
   (`tools/replay-eval/.worktrees/<run-id>/`), builds it, and starts it on an
   ephemeral port. It never touches the operator's working tree or running
   service.
2. It replays each fixture's turns against the candidate and the baseline,
   scores both, and writes `score.json` + `report.md`.
3. The score report is **attached to the candidate's PR** (as a comment or
   uploaded artifact). A reviewer merges the PR only after inspecting the
   score delta.
4. The worktree + ephemeral process are torn down after the run. No candidate
   state persists.

**Forbidden:**

- Mutating `main` or any shared branch directly.
- Running the candidate against a production DB or the operator's live
  service.
- Promoting a candidate without a score report on its PR.

This is the "controlled" in controlled self-evolution: an automated agent may
*prepare* a candidate branch + score, but a human (or a separately-reviewed
automation) *merges* it after reading the report.

## 6. score.json

Machine-readable, one per replay run:

```jsonc
{
  "meta": {
    "generated_at": "2026-06-18T12:00:00Z",
    "candidate": "feat/my-change",
    "candidate_commit": "def456...",
    "baseline": "main",
    "baseline_commit": "abc123...",
    "fixture_count": 12
  },
  "summary": {
    "candidate_score": 0.92,        // 0.0–1.0 weighted pass rate
    "baseline_score": 0.88,
    "delta": 0.04,                  // candidate - baseline; positive = improvement
    "verdict": "improve"            // "improve" | "regress" | "neutral"
  },
  "fixtures": [
    {
      "fixture_id": "feishu-greeting-001",
      "candidate": { "score": 1.0, "passes": 5, "fails": 0, "details": [ /* per-expectation */ ] },
      "baseline":   { "score": 1.0, "passes": 5, "fails": 0, "details": [ /* per-expectation */ ] },
      "delta": 0.0,
      "verdict": "neutral"
    }
  ]
}
```

Scoring rules:

- Each expectation is binary (pass/fail) for the MVP. `score = passes / total`.
- `verdict` per fixture: `improve` (candidate > baseline), `regress`
  (candidate < baseline), `neutral` (equal).
- Overall `verdict`: `regress` if any fixture regresses and the aggregate
  delta < 0; `improve` if aggregate delta > 0 with no regressions; else
  `neutral`.

A hard-fail set (independent of score) aborts the run and forces a `regress`
verdict: duplicate reply, a forbidden operation emitted, a policy denial when
`policy_verdict: "allow"` was expected, or a candidate crash.

## 7. report.md

Human-readable, mirrors `score.json` with prose:

```markdown
# Replay/Eval Report

Candidate: feat/my-change (def456)  Baseline: main (abc123)

## Summary

Verdict: **improve** (candidate 0.92 vs baseline 0.88, Δ +0.04)

## Per-fixture

| Fixture | Candidate | Baseline | Δ | Verdict |
|---|---|---|---|---|
| feishu-greeting-001 | 1.00 | 1.00 | 0.00 | neutral |
| ... | ... | ... | ... | ... |

## Regressions

(none)

## Improvements

- feishu-tool-call-003: 0.6 → 1.0 (candidate now emits time.now correctly)

## Hard failures

(none)
```

## 8. Replay Mechanics

For each fixture:

1. Start (or reuse) a candidate build on an ephemeral port with an ephemeral
   `--db` path (a fresh temp file; never the production DB).
2. For each turn: POST the ingress envelope to `/v1/ingress`, then poll
   `/health` (or `/v1/...`) until the run reaches a terminal status
   (`Completed`/`Failed`/`Unknown`) or a timeout.
3. Read the resulting journal via the **Audit Harness** (`tools/audit-report`)
   against the ephemeral DB to extract: emitted operations, reply text,
   duplicate-reply count, policy verdict, latency.
4. Score each expectation.
5. Tear down the ephemeral DB + process.

The Replay/Eval harness **composes with** the Audit harness — it does not
re-implement journal reading. This keeps the boundary clean: audit = read,
replay/eval = drive + score (using audit to read).

## 9. Safety & Non-Goals

- **Not a fuzzer.** Fixtures are curated, not generated at scale.
- **Not a model trainer.** The harness scores; it does not update weights.
- **Not unattended.** Promotion requires a PR with a score report. An automated
  agent may open the PR, but merge is gated.
- **No production touch.** Ephemeral DB, ephemeral port, temporary worktree.
- **No new Kernel module.** All logic is in `tools/` or the external package.
- The MVP does **not** auto-generate fixtures from production runs — that is a
  later increment and must itself be audited for secret/data-leak risk before
  it is allowed.

## 10. Implementation Plan (post-design)

1. **Scaffold** `tools/replay-eval/` (CLI + fixture validator).
2. **Worktree runner** — check out candidate/baseline, build, start on
   ephemeral port with temp DB.
3. **Replay driver** — POST turns, poll to terminal, invoke audit-report to
   read results.
4. **Scorer** — per-fixture expectation evaluation → `score.json`/`report.md`.
5. **Synthetic fixtures** — 2–3 hand-authored regression fixtures (no secrets).
6. **Extraction prep** — document the `agent-core-replay-eval` target.

Each step is its own small PR; this design merges first.

## 11. Validation (this PR, doc-only)

```text
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs
git diff --check
```
