# Agent Core — External Self-Evolution Rehearsal Harness (skeleton)

Strings goal → candidate branch → (planned) replay/eval + audit → report into an
experienceable loop. **DRY-RUN BY DEFAULT.** Manual merge only.

**This tool lives outside `src/` and is not a Kernel dependency.** It composes
(`tools/audit-report`, `tools/replay-eval`); it does not re-implement them.
See `docs/evolution-harness.md` for the design contract.

## Hard safety rules

- **Dry-run by default.** Produces `plan.json` + `evolution-report.md` without
  spawning a worker agent, committing, merging, or pushing. Even with
  `--no-dry-run`, **merge is always manual**.
- Refuses forbidden paths: `.env`, `.agent-core`, `~/.openduck`, `~/.openclaw`,
  `logs`, production DB.
- Refuses unsafe git refs (shell metacharacters, path traversal).
- Never invokes `git push` / `git merge`. There is no such call in the code.
- No Kernel `src/` writes; no workflow/multi-agent/shell/browser/deploy.

## Usage

```bash
node --experimental-strip-types tools/evolution-harness/cli.ts \
  --goal docs/current-goal.md \
  --candidate feat/my-change \
  --base main \
  --out-dir ./out
#   optional: --fixtures-dir <dir>  --audit-db <copied.db>  --no-dry-run
```

Writes to `out/<run-id>/`: `plan.json`, `evolution-report.md`, `manifest.json`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | report written |
| 2 | `--goal` or `--candidate` missing |
| 3 | forbidden path / unsafe ref / unresolvable ref / missing file |

## Tests

```bash
node --test --experimental-strip-types tools/evolution-harness/test/*.test.ts
```
