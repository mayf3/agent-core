# Harness Acceptance Runbook

How to accept that Agent Core's external harnesses actually work — without
taking the agent's word for it. This is the operator/reviewer's manual checklist.

**These harnesses are external.** They live in `tools/` (audit-report,
replay-eval) and are **not** part of the Rust Kernel Runtime — they never appear
under `src/` and the Kernel never depends on them. They read copied snapshots
or drive ephemeral builds; they never touch production SQLite, live services,
`.env`, `~/.openduck`, `~/.openclaw`, logs, or secrets.

---

## 1. Audit Report (`tools/audit-report`)

Inspect a **copied** SQLite snapshot of the Kernel journal.

### Run it

```bash
# 1. Copy a safe snapshot (operator's responsibility — never the live DB):
cp /var/lib/agent-core/journal.db /tmp/audit-snapshot.db

# 2. Run:
node --experimental-strip-types tools/audit-report/cli.ts \
  --db /tmp/audit-snapshot.db \
  --out-dir ./out
#   optional: --health /path/to/health.json  --git-rev abc123
```

Writes `out/report.md` + `out/report.json` (8 sections).

### Read it

- **§2 Hash-chain status** — `integrity` must be `ok`. If `faulty`, the journal
  is corrupted; **stop and investigate**.
- **§4 Unknown dispatches** — `count` should trend to 0 (unacked unknown
  dispatches are recoverable but signal a prior crash).
- **§5 Projection drift** — `count` should be 0; non-zero means an outbox
  row's projection status disagrees with its terminal Journal fact.
- **§6 Undelivered ingress** — `count` should be 0; non-zero means an ingress
  was accepted but never delivered (the Kernel would re-queue on restart).
- **§7 Approval waits/expiries** — only non-zero when
  `AGENT_CORE_REQUIRE_WRITE_APPROVAL=true`.

### Acceptance checklist (audit)

- [ ] The report runs against a **copied** snapshot, not the live DB.
- [ ] `hash_chain.integrity === "ok"`.
- [ ] No unacked unknown dispatches (`unknown_dispatches.count` explained or 0).
- [ ] No projection drift (`projection_drift.count === 0`).
- [ ] No undelivered ingress (`undelivered_ingress.count === 0`).
- [ ] No duplicate-reply idempotency collisions
      (`duplicate_reply_safety.idempotency_collisions === 0`).

---

## 2. Replay/Eval (`tools/replay-eval`)

Evaluate a **candidate** branch against a baseline through curated fixtures.

### Run it (single fixture)

```bash
node --experimental-strip-types tools/replay-eval/cli.ts \
  --fixture tools/replay-eval/examples/smoke.json \
  --candidate feat/my-change \
  --baseline main \
  --out-dir ./out
```

### Run it (suite — every fixture in a dir, one aggregated report)

```bash
node --experimental-strip-types tools/replay-eval/cli.ts \
  --fixtures-dir tools/replay-eval/examples \
  --candidate feat/my-change \
  --baseline main \
  --out-dir ./out
```

`--fixture` and `--fixtures-dir` are mutually exclusive. The candidate +
baseline are built once (in temp git worktrees) and reused across fixtures.
All resources are ephemeral (fresh temp DB, ephemeral port, temp worktree).

### Read `score.json` / `report.md`

- **summary.verdict** — `improve` / `neutral` / `regress`. **`regress` blocks
  merge.**
- **summary.delta** — candidate score minus baseline score.
- **fixtures[]** — per-fixture candidate/baseline score, delta, verdict,
  `hardFail`.
- **Candidate hard failures** section (markdown) — the failing expectation
  details for any hard-failing fixture.

### Acceptance checklist (replay/eval)

- [ ] The harness built the candidate in a **temp worktree**, not your tree.
- [ ] It used a **fresh temp DB + ephemeral port** (not production).
- [ ] `summary.verdict !== "regress"` (or the regression is reviewed + accepted).
- [ ] No fixture has `candidate.hardFail === true` unless intentionally testing
      a negative case.
- [ ] `report.md` "Candidate hard failures" is empty (or the failures are
      explained).

---

## 3. Red-lines that BLOCK merge

Regardless of score, **do not merge a candidate** if any of these is true:

| Red-line | Where seen |
|---|---|
| Hash-chain `faulty` | audit-report §2 |
| Unacked unknown dispatch (`status='unknown' AND acked_unknown=0`) | audit-report §4 |
| Projection drift (projection status ≠ terminal Journal fact) | audit-report §5 |
| Undelivered ingress (`payload.event_id` not in delivered set) | audit-report §6 |
| Duplicate-reply idempotency collision | audit-report §8 |
| Replay `summary.verdict === "regress"` | replay-eval score.json |
| Replay candidate `hardFail === true` | replay-eval score.json |

These are objective, machine-checkable blockers. A reviewer should be able to
point at the report line that triggers each one.

---

## 4. Boundary reminder

- External harnesses, **not** Kernel Runtime. They do not enter `src/`.
- Read-only (audit) or ephemeral (replay). Never mutate the production Journal,
  approval state, or connector state.
- No `.env` / `~/.openduck` / `~/.openclaw` / logs / secrets / live DB.
- Promotion is **manual** (PR only). The harnesses produce reports; they never
  merge, push, or open PRs.

---

## 5. What "done" looks like for a candidate

A candidate is accept-ready when:

1. The audit report against a fresh snapshot passes every §3 red-line.
2. The replay/eval suite verdict is not `regress` and no fixture hard-fails
   (or each hard-fail is a documented intentional negative case).
3. A reviewer can reproduce both reports from the commands above.
