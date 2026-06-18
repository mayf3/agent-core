# Feishu Connector

The Feishu Connector is a **channel adapter** for the Agent Core Kernel. It
maintains a long-running WebSocket connection to Feishu (Lark), normalizes
incoming chat events, and forwards them to the Kernel via `POST /v1/ingress`.
When the Kernel decides to reply, it sends a `POST /v1/execute` request that
the connector fulfills by calling the Feishu Send Message API.

This connector is **not** a Kernel, Runtime, Gateway, Journal, Session
manager, LLM caller, Context assembler, Policy engine, or Agent loop. It is
the thinest possible translation layer between the Feishu wire protocol and
the Kernel's IPC protocol.

## Architecture

```text
Feishu WS Event  ──►  normalizeMessageEvent()  ──►  POST /v1/ingress
                                                         │
                                                    Kernel decides
                                                    to reply
                                                         │
                                                   POST /v1/execute
                                                         │
                                                    sendReply()
                                                    (feishu.send_message)
```

### Startup Sequence

1. `loadConfig()` reads env vars (see `config.ts` for full list).
2. `createJsonlExecuteStore(config.executeStatePath)` creates a JSONL-backed
   execute idempotency store and preloads it via `.load()`.
3. `createReactionTracker(config, client)` creates a reaction-state tracker
   and loads persisted reaction markers.
4. `startExecuteServer(config, client, reactions, executeStore)` starts the
   `/v1/execute` HTTP server.
5. Lark WebSocket client starts listening for incoming messages, which are
   dispatched to `postIngress()`.

## Idempotency Boundaries

| Direction | Idempotency Mechanism | Owner | Detail |
|-----------|----------------------|-------|--------|
| Ingress (Feishu → Kernel) | Kernel Journal + `ingress_dedup` | Rust Kernel | Dedup key = `external_event_id` = `message:<messageId>`. The Kernel guarantees at-most-once acceptance. |
| Execute (Kernel → Connector → Feishu) | Connector-local JSONL store | Feishu Connector | Dedup key = `idempotency_key`. After successful `sendReply`, a `"sent"` record is appended to `feishu-executes.jsonl`. On restart the store is reloaded; replayed keys short-circuit without re-calling the Feishu API. |
| Reaction state | Connector-local JSONL store | Feishu Connector | Processing/failed reaction markers are persisted in `feishu-reactions.jsonl` and recovered after restart. |

### Execute dedup flow (within same process)

```text
POST /v1/execute { idempotency_key: "k1" }
  │
  ├─ inFlight.has("k1")? ──yes──► return { replayed: true }  (concurrent)
  │
  ├─ store.get("k1")?.status === "sent"? ──yes──► return { replayed: true }
  │
  └─ sendReply → success → store.set("k1", "sent") → return { receipt }
                          → inFlight cleaned up in .finally
```

## Persistence

All connector-local state uses **append-only JSONL** files under
`AGENT_CORE_DATA_DIR` (default `~/.agent-core/`). No SQLite, no production
database access.

| File | Content | Env Override | TTL | Compaction |
|------|---------|-------------|-----|-----------|
| `feishu-executes.jsonl` | Sent execute records (`idempotencyKey`, `invocationId`, `status`, `receiptSummary`) | `AGENT_CORE_FEISHU_EXECUTE_STATE_PATH` | 7 days | 256 KB |
| `feishu-reactions.jsonl` | Reaction markers (`messageId`, `reactionId`, `status`) | `AGENT_CORE_FEISHU_REACTION_STATE_PATH` | None (swept on delete) | 256 KB |

## Configuration

All configuration comes through environment variables. The connector **never**
reads `.env`, `~/.openduck`, `~/.openclaw`, logs, production databases, or
secret files directly.

See `src/config.ts` for the complete list, including `appId`, `appSecret`,
`ipcToken`, `connectorPort`, reaction retry parameters, and execute state
path.

## Boundaries

The Feishu connector **is allowed to**:

- Maintain a Lark WebSocket long connection
- Normalize incoming Feishu events into the Kernel's ingress schema
- POST normalized events to `POST /v1/ingress`
- Listen for `POST /v1/execute` and call `feishu.send_message`
- Persist execute idempotency and reaction state to JSONL files
- Add/remove reaction emoji on Feishu messages

The Feishu connector **is NOT allowed to**:

- Implement Runtime, Session, LLM, Context, Policy, or Agent Loop logic
- Read or write Kernel Journal, outbox, runs, or sessions tables
- Call any Kernel IPC endpoint other than `POST /v1/ingress`
- Execute operations other than `feishu.send_message`
- Import from `src/` (Kernel code)
- Access `.env`, `~/.openduck`, `~/.openclaw`, logs, or production databases
- Introduce Workflow, Multi-Agent, Shell, Browser, or other non-Feishu
  capabilities

## Connection to Kernel

The Rust Kernel does **not** import or depend on `connectors/feishu/`. It
communicates with the connector exclusively through the IPC protocol
(HTTP `POST /v1/ingress` from connector to Kernel, HTTP `POST /v1/execute`
from Kernel to connector). The Kernel treats the connector as a stateless
edge adapter; all durable state lives in the Kernel's SQLite journal or in
the connector's JSONL store.

## Running

```bash
# Development (from repo root)
pnpm feishu-connector

# Tests
pnpm check:connector
```

## Testing

All connector tests use **ephemeral temp directories** and fake HTTP servers
with port 0 (OS-assigned). No production data, no secrets, no network calls
to real Feishu.

- `src/execute-store.test.ts` — JSONL persistence, TTL, compaction, secret
   leak
- `src/execute-server.test.ts` — execute validation, idempotency dedup
   (in-flight, persisted, restart), failure mode, secret leak
- `src/reactions.test.ts` — reaction lifecycle, retry, loading persisted
   state
- `src/reaction-store.test.ts` — JSONL reaction store, compaction
- `src/kernel.test.ts` — event normalization, dedup key derivation
- `src/safe-logger.test.ts` — secret redaction in logs
