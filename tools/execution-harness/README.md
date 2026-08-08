# execution-harness

A minimal, generic execution primitive for persistent agents, exposed
through the Kernel's existing external-capability protocol.

It is an **independent harness** — not the Coding Harness, not the Route
Harness, and not a coding/route/failure-specific tool. It provides only
generic, isolated workspace primitives:

| operation | contract |
|---|---|
| `external.coding_workspace_list` | list files in an isolated workspace |
| `external.coding_workspace_read` | read a file (path-fenced) |
| `external.coding_workspace_write` | write a file (path-fenced) |
| `external.coding_workspace_exec` | run a command: `workspace.exec(command, cwd, timeout)` — shell / git / build / test / local process / HTTP probe |

The operation names live in the `external.coding_workspace_*` namespace
because the Kernel's hardcoded grant allowlist
(`OWNER_DEVELOPMENT_OPERATIONS`, `src/domain/coding_operations.rs`)
auto-grants exactly those names to the configured coding owner; the
implementation is a new generic harness, unrelated to the Coding Harness.

## Security model

- loopback-only listener (`EXECUTION_HARNESS_LISTEN_ADDR`, default
  `127.0.0.1:7650`)
- mandatory bearer token (`EXECUTION_HARNESS_TOKEN`); unauthenticated
  requests get `401` — fail closed, the service refuses to start without
  a token
- every file path is canonicalized and must resolve inside the workspace
  root (`..` escapes, absolute paths, and symlink escapes are rejected)
- command environment is cleared (`env_clear`) except
  `PATH`/`HOME`/`TMPDIR` and `LANG`/`LC_*`
- per-command timeout (default 30s, hard cap 120s); the whole process
  group is killed on timeout
- output caps (default 32 KiB per stream, hard cap 64 KiB) keep responses
  inside the Kernel adapter's 64 KiB limit
- no production secrets: the harness reads only its own
  `EXECUTION_HARNESS_*` variables; it never sources `runtime.env` and has
  no deployment privileges

## Environment

```bash
EXECUTION_HARNESS_LISTEN_ADDR=127.0.0.1:7650
EXECUTION_HARNESS_TOKEN=<shared with the Kernel adapter bearer token>
EXECUTION_HARNESS_WORKSPACE_ROOT=/path/to/isolated/workspaces
```

The Kernel adapter sends every external-harness call with the bearer
`AGENT_CORE_CAPABILITY_HOST_EXECUTION_TOKEN`; configure
`EXECUTION_HARNESS_TOKEN` to the same value so the harness authenticates
the Kernel.

## Protocol

Request (as sent by the Kernel adapter):

```json
{"protocol_version":"external-harness-v1","invocation_id":"...",
 "operation":"external.coding_workspace_exec","arguments":{...}}
```

Response:

```json
{"protocol_version":"external-harness-v1","ok":true,"result":{...}}
```

Rejections are `ok:false` with a bounded `error_code`
(e.g. `path_escape`, `missing_command`, `program_not_found`,
`workspace_create_failed`, `unknown_operation`). Authentication failures
return HTTP `401`.

## Registering with the Kernel (no Kernel code change)

The Kernel exposes `POST /v1/harness/register` (Bearer
`AGENT_CORE_IPC_TOKEN`) followed by `POST /v1/harness/enable` — see
`docs/execution-harness-registration.md` for the exact payloads. After
registration the configured owner sees the new tools in its next run's
ToolCatalog and can call them through the normal tool loop.

## Tests

```bash
cargo test            # unit tests (fencing, timeout, protocol) + HTTP e2e
```
