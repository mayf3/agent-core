# Agent Core — Replay/Eval Harness (MVP)

Replays a curated fixture through a **candidate** Kernel build and a **baseline**
build, scores each against soft expectations, and writes `score.json` +
`report.md`. This is the gate that makes "controlled self-evolution" safe: a
candidate only reaches `main` via a PR that carries its own score report.

**This tool lives outside `src/` and is not a Kernel dependency.** Long-term
extraction target: `agent-core-replay-eval`. See
`docs/replay-eval-harness.md` for the design contract.

## Hard safety rules

- **Ephemeral everything**: fresh temp DB, ephemeral port, temporary git
  worktree. Never the production DB, never the operator's running service,
  never the operator's working tree.
- The harness is a **client** of the Kernel over HTTP (`/v1/ingress`,
  `/health`). It does not link Kernel code or mutate Journal/approval state.
- Does **not** read `.env`, `.agent-core`, `~/.openduck`, `~/.openclaw`, logs,
  API keys, tokens, or Authorization from any committed file. The Kernel's IPC
  token is generated fresh per run; the model key passed to the candidate is a
  stub (the `LocalEchoLlm` build ignores it). The candidate process receives a
  **minimal explicit env only** (the harness does **not** spread `process.env`,
  so the operator's real secrets never leak into the candidate).
- **Promotion is manual**: this tool only produces a score — it never merges,
  never pushes to `main`, never opens a PR on its own. A reviewer merges after
  reading the report.

## Usage

Run a **single fixture**:

```bash
node --experimental-strip-types tools/replay-eval/cli.ts \
  --fixture tools/replay-eval/examples/smoke.json \
  --candidate feat/my-change \
  --baseline main \
  --out-dir ./out
```

Or a **suite** (every `*.json` fixture in a directory, one candidate/baseline
build reused across all, one aggregated report):

```bash
node --experimental-strip-types tools/replay-eval/cli.ts \
  --fixtures-dir tools/replay-eval/examples \
  --candidate feat/my-change \
  --baseline main \
  --out-dir ./out
```

`--fixture` and `--fixtures-dir` are mutually exclusive (exactly one required).
Writes `out/score.json` and `out/report.md`.

## What it does (per run)

1. Resolves `--candidate` and `--baseline` git refs.
2. Checks each out into a **temporary git worktree** and builds the Kernel
   binary (`cargo build --release`).
3. For the fixture: starts the candidate on an **ephemeral port** with a
   **fresh temp DB**, POSTs each turn to `/v1/ingress`, polls `/health`, then
   reads the ephemeral DB (read-only) to extract the outcome.
4. Repeats against the baseline.
5. Scores each outcome against the fixture's expectations, compares, and writes
   the report.
6. Tears down the worktrees + temp DBs (always, in a `finally`).

## Score model

- Each expectation is binary; `score = passes / total`.
- Per-fixture verdict: `improve` / `regress` / `neutral`.
- **Hard-fail set** (forces `regress` regardless of score): duplicate reply, a
  forbidden operation emitted, a policy denial when `allow` was expected, or a
  candidate crash.

## Fixture format

See `docs/replay-eval-harness.md §4` and `examples/`. Fixtures are
deterministic seeds with **soft** expectations (`reply_contains_any`, not exact
match). Empty `expectations` = smoke replay. The shipped fixture pack exercises
the expectation kinds: `forbidden_operations`, `policy_verdict`, and
`reply_contains_any` (see `test/replay.test.ts`).

**Hard-fail details are always structured.** Any expectation that forces a
`regress` verdict (duplicate reply, forbidden operation, policy-deny-when-allow,
crash) pushes a structured `ExpectationResult` object into `details` — never a
bare boolean. This is covered by per-branch regression tests plus a
cross-cutting invariant test.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | report written; verdict printed to stdout |
| 1 | unexpected fatal |
| 2 | `--fixture` or `--candidate` missing |
| 3 | `--fixture` not a regular file / invalid fixture JSON |
| 4 | driver error (worktree/build/replay) |

## Tests

```bash
node --test --experimental-strip-types tools/replay-eval/test/replay.test.ts
```

The unit tests cover fixture validation + scoring (54 tests). The end-to-end
driver (build + start + replay) requires `cargo` + `git` and is exercised
manually; it is intentionally not part of the `node --test` suite to keep CI
fast and dependency-free.
