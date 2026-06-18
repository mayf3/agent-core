# Agent Core — External Self-Evolution Rehearsal Harness

Strings goal → candidate ref → (planned) replay/eval + audit → evidence package
→ pass/blocked decision into an experienceable loop. **Plan-only by default.**
Manual merge only.

**Current capability:** plan + evaluate (with `--evaluate` + `--fixtures-dir` and/or
`--audit-db`): resolves + pins the candidate/base commits, validates all inputs,
composes `tools/replay-eval` and `tools/audit-report` as subprocesses, and
derives a `pass`/`blocked` decision from the red-lines. Emits `plan.json` +
`evolution-report.md` + `manifest.json`. Merge is always manual.

**This tool lives outside `src/` and is not a Kernel dependency.** It composes
(`tools/audit-report`, `tools/replay-eval`); it does not re-implement them.
See `docs/evolution-harness.md` for the design contract.

## Hard safety rules

- **No shell.** All git calls use `spawnSync` + argv (never string-concatenated
  `execSync`).
- Refuses forbidden paths: `.env`, `.agent-core`, `~/.openduck`, `~/.openclaw`,
  `logs`, production DB.
- Refuses unsafe git refs: shell metacharacters, path traversal (`..`),
  control characters, leading `-` (option injection), empty, or unresolvable.
- Validates `--fixtures-dir` is an existing directory; `--audit-db` is an
  existing regular file (a copied snapshot, never the live DB).
- Never invokes `git push` / `git merge`. There is no such call in the code.
- No Kernel `src/` writes; no workflow/multi-agent/shell/browser/deploy.

## Usage

```bash
# Plan-only (default):
node --experimental-strip-types tools/evolution-harness/cli.ts \
  --goal docs/current-goal.md \
  --candidate feat/my-change \
  --base main \
  --out-dir ./out
#   optional: --fixtures-dir <dir>  --audit-db <copied.db>  --evaluate
```

`--no-dry-run` is rejected (`not_implemented`); use `--evaluate` for real
evaluation (merge stays manual).

Writes to `out/<run-id>/`: `plan.json`, `evolution-report.md`, `manifest.json`
(+ `replay/` and/or `audit/` when `--evaluate` is used).

## Exit codes

| Code | Meaning |
|---|---|
| 0 | report written, decision is **pass** |
| 2 | `--goal` or `--candidate` missing / `--evaluate` without `--fixtures-dir` and `--audit-db` |
| 3 | forbidden path / unsafe ref / unresolvable ref / missing file / `--fixtures-dir` not a directory |
| 4 | `--no-dry-run` (not implemented) |
| 5 | harness internal error (spawn failure, timeout) |
| 10 | evaluation **blocked** by red-line (replay regress/hardFail or audit fault) |

## Tests

```bash
node --test --experimental-strip-types tools/evolution-harness/test/*.test.ts
# or, via the repo gate:
pnpm check:harnesses
```
