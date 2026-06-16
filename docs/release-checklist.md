# Release Checklist

Phase 1 Operational Hardening. This is the pre-release checklist an operator
runs before cutting or deploying an Agent Core build. Every item maps to a
concrete, verifiable command or invariant on `main` — no aspirational steps.

## 1. Build & test gates (all must pass)

Run from the repo root:

```bash
cargo build
cargo test
pnpm check
```

`pnpm check` is the authoritative composite gate. It runs, in order:

- `node scripts/check-structure.mjs` — file/dir layout invariants (≤ 500 lines
  per file, ≤ 20 files per dir, required anchors present).
- `node scripts/check-local-secret-leaks.mjs` — scans for leaked secrets /
  credentials. Must report `secret scan passed`.
- `cargo test` — all Rust suites (currently 14 suites).
- `pnpm check:connector` — TypeScript Feishu connector tests + module import
  smoke (currently 13 tests + `connector modules ok`).

A release is blocked if any of these fails. CI may not be wired, so these are
run locally before tagging.

## 2. Layout validation

```bash
python3 docs/m5-outbox-stabilization/validation_layout.py
```

Must print `validation_layout: OK`. This catches regressions in required code
anchors (e.g. the `parse_kind` `_ => JournalEventKind::Unknown` sentinel, the
schema-version check, the dispatcher-observability fields).

## 3. Whitespace / diff hygiene

```bash
git diff --check
```

Must be clean (no trailing whitespace, no conflict markers).

## 4. Secret / credential boundary

- No `.env`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.crt` files committed.
- No `~/.agent-core`, `~/.openduck`, `~/.openclaw`, or runtime log paths in the
  diff.
- API keys / tokens are loaded from env vars at runtime, never baked into code
  or committed.

## 5. Schema compatibility

- `migrations/` — if a new migration was added, `CURRENT_SCHEMA_VERSION` in
  `src/journal/sqlite.rs` must be bumped to match, and a forward-version test
  in `tests/m1_schema_version.rs` must cover the new version.
- Existing on-disk databases at `user_version = N` must still open cleanly with
  the new binary (the startup `migrate()` skips the base migration for known
  versions and only rejects DBs newer than `CURRENT_SCHEMA_VERSION`).

## 6. Health surface (`/health`)

After a deploy, `GET /health` must respond 200 and report:

- `"status"`: `"ok"` (or `"degraded"` / `"corrupt"` if the system is degraded —
  these are expected post-incident, not at a clean release).
- `"hash_chain_ok": true` — Journal integrity intact.
- `"outbox_dispatcher_running": true` — the dispatcher loop thread is alive.
- `"outbox_unknown_count": 0` and `"outbox_stale_dispatching_count": 0` at
  steady state (non-zero after a crash is recoverable, not a release blocker).

A release that cannot reach `status: ok` on a fresh DB after a clean restart is
blocked.

## 7. Recovery invariants (must hold)

These are enforced by tests but must be re-affirmed before release:

- **No automatic redispatch.** `unknown` outbox rows and stale dispatching rows
  are never auto-retried. Recovery reconciles the projection to the Journal
  terminal fact only (no duplicate Journal events, no adapter calls).
- **Journal is the source of truth.** `worker_jobs` / `outbox_dispatches` are
  projections; they can be rebuilt from the Journal.
- **Terminal transition guard.** `succeed` / `fail` / `unknown_outbox_dispatch`
  reject any row not in `status = 'dispatching'`.

## 8. Boundary check

- The Rust Kernel is the only Runtime / Gateway / Journal.
- The TypeScript Feishu Connector is an edge adapter — it does not own Runtime,
  Gateway, or Journal state.
- No Workflow / Multi-Agent / Shell / Memory / Dynamic Hook / Plugin / Sandbox
  / Self-Evolution code is introduced prematurely.

## 9. Tagging

Once 1–8 pass on `main`:

```bash
git tag -a v0.X.Y -m "..."
git push origin v0.X.Y
```

The tag is the release artifact. There is no separate build pipeline; the
binary built from the tagged commit is the release.
