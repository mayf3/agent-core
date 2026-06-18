# Connector Extraction Checklist

This checklist captures the preconditions for extracting the Feishu connector
out of this repo into a standalone package or service. It is the Phase 3 gate
documented in `docs/decisions/connector-local-durability.md`.

A connector is an edge adapter — it translates between an external channel
(Feishu, CLI, etc.) and the Kernel's internal protocol. It is **not** a
Runtime, Gateway, Journal, Session manager, LLM caller, Context assembler,
Policy engine, or Agent loop.

## Current Status

| # | Item | Status | Notes |
|---|------|--------|-------|
| 1 | **Execute idempotency persistence** | ✅ Done (PR #139) | JSONL append-log, 7d TTL, compaction; survives connector restart. |
| 2 | **Eviction / TTL** | ✅ Done (PR #139) | Built into execute store: `maxAgeMs` (default 7d), load-time sweep, compaction at 256 KB. |
| 3 | **Regression tests** | ✅ Done (PR #139) | HTTP integration tests: restart replay, same-server dedup, failure exclusion, secret leak. |
| 4 | **Idempotency contract documented** | ❌ Open | IPC doc must state: execute idempotency = connector-local JSONL store; ingress idempotency = Kernel Journal + `ingress_dedup`. |
| 5 | **No kernel-connector import cycle** | ✅ By design | Rust Kernel does not import `connectors/`. TS connector does not import `src/`. Verify with `node scripts/check-structure.mjs`. |
| 6 | **Test suite portability** | ✅ In progress | All connector tests use ephemeral temp fixtures. No production DB or secret files. Run via `pnpm check:connector`. |
| 7 | **Secret scan passes** | ✅ Continuous | `node scripts/check-local-secret-leaks.mjs` covers all tracked files. |

## Before Extraction

When the Feishu connector is ready to move to its own repo, the extractions PR
must also include:

1. **Remove implicit dependency on `AGENT_CORE_DATA_DIR`** — the standalone
   connector should have its own env namespace (e.g. `FEISHU_CONNECTOR_*`).
2. **Remove implicit dependency on `AGENT_CORE_IPC_TOKEN`** — the IPC token
   should be configurable independently.
3. **Preserve all existing tests** in the new repo (they must pass before
   and after extraction).
4. **Vendor or re-export `@larksuiteoapi/node-sdk`** — the extraction must
   not break the Kernel build by removing a dependency from `package.json`
   that the Kernel doesn't actually use at runtime.
5. **Update this doc** to link to the new repo location and mark extraction
   complete.

## Architecture Invariants

These invariants must hold **before and after extraction**:

- **Ingress dedup** is owned by the Rust Kernel Journal (`journal_events`
  + `ingress_dedup` table). The connector sends `external_event_id` per
  message; the Kernel guarantees at-most-once acceptance. The connector
  does not deduplicate ingress.
- **Execute idempotency** is owned by the connector-local JSONL store
  (`connectors/feishu/src/execute-store.ts`). The Kernel generates an
  `idempotency_key` per outbox dispatch; the connector persists the `"sent"`
  status after successfully calling the Feishu API. On restart the connector
  reloads the store and short-circuits replayed keys without re-calling the
  Feishu API.
- **Reaction state** is owned by the connector-local JSONL store
  (`connectors/feishu/src/reaction-store.ts`). The connector persists
  processing/failed reaction markers and recovers them after restart.
- **The connector never reads `.env`, `~/.openduck`, `~/.openclaw`,
  logs, production databases, or secret files.** All configuration comes
  through environment variables defined in `config.ts`.
- **The connector only speaks Feishu.** It maintains a WebSocket long
  connection, normalizes incoming events, POSTs them to
  `POST /v1/ingress`, and responds to `POST /v1/execute` by calling
  `feishu.send_message`. No other channel, operation, or protocol.

## What The Connector Is Not

The Feishu connector is explicitly **not** any of the following. These
capabilities live in the Rust Kernel or future dedicated components:

| Capability | Owner | Reason |
|-----------|-------|--------|
| Session management | Rust Kernel (`src/`) | State machine, journal, projection |
| LLM invocation | Rust Kernel (`src/`) | Model routing, retry, fallback |
| Tool-call policy / approval | Rust Kernel (`src/`) | Gateway, catalog, grant check |
| Context assembly | Rust Kernel (`src/`) | File loading, truncation, prompt |
| Agent loop / routing | Rust Kernel (`src/`) | Run lifecycle, dispatch |
| Journal / hash chain | Rust Kernel (`src/`) | Append-log, integrity, recovery |
| Replay / eval harness | `tools/` (TS/JS) | External harness, not in process |
| Workflow / DAG | Not implemented | Out of scope per `product-roadmap.md` |
| Multi-agent orchestration | Not implemented | Out of scope per `product-roadmap.md` |
| Shell execution | Not implemented | Requires dedicated adapter |
| Browser automation | Not implemented | Requires dedicated adapter |

## Verification Commands

Before considering extraction complete, run:

```bash
# Connector test suite
pnpm check:connector

# Full project checks
node scripts/check-structure.mjs
node scripts/check-local-secret-leaks.mjs

# Kernel tests
cargo test

# No import dependency from connector to src/
# (verified by structure check — connectors/ is outside src/)
```

## Reference

- `docs/decisions/connector-local-durability.md` — decision record and
  original Plan B design
- `connectors/feishu/src/execute-store.ts` — execute idempotency persistence
- `connectors/feishu/src/reaction-store.ts` — reaction state persistence
- `connectors/feishu/src/execute-server.ts` — `/v1/execute` handler with
  in-flight + persisted dedup
- `connectors/feishu/src/kernel.ts` — ingress normalization and POST
- `connectors/feishu/src/config.ts` — all env-var configuration
- `connectors/feishu/README.md` — connector overview and boundaries
