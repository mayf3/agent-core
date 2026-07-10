# Harness Smoke Runner v0

> **Audience**: Agent Core operators, developers, and agents
> **Prerequisites**:
> - Node.js 18+
> - An external Harness project (generated via `scaffold-context-harness.sh` or written manually)
> - Harness server running locally (user-managed)

## Overview

**Smoke Runner v0** (`smoke-context-harness.mjs`) is a local verification tool
for external `context.prepare.v0` Harnesses. It reads the Harness manifest,
runs the declared smoke command, checks the health endpoint, and validates
the `context.prepare.v0` hook endpoint — then outputs a text readiness report
with manual registration suggestions.

### What it does

Smoke Runner performs six sequential checks:

1. **Manifest validation** — verifies `harness.manifest.json` schema and required fields
2. **Local-only endpoint check** — confirms both `endpoint.url` and `health.url`
   are loopback (`127.0.0.1`, `localhost`, or `[::1]`)
3. **Smoke command** — runs the manifest's `smoke.command` (typically `npm test`)
4. **Health check** — `GET` the `health.url` endpoint, expects `200` + `{"status":"ok"}`
5. **Context.prepare check** — `POST` a minimal `HookRequestEnvelope` to the
   hook endpoint, validates the response
6. **Expected fragment check** — optional; verifies that at least one fragment
   content contains the `--expect-fragment` string

### What it does NOT do

Smoke Runner **does not**:

- Register the Harness with the Kernel
- Modify `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL` or any other env var
- Enable, disable, or configure any Kernel hook
- Restart the Kernel
- Start or stop the Harness server
- Deploy or restart any service
- Modify the Feishu Connector
- Modify Kernel production code
- Modify the DB
- Expose coding tools to non-owner users
- Write to `~/.agent-core/harnesses/`

`READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION` only means local smoke checks
pass — it does **not** mean the Harness is registered or connected to the
Kernel. Registration is a separate, manual step.

The operator must start the Harness server manually before running the
smoke runner. The runner only performs read-only checks and prints
suggestions for manual registration.

## Usage

### 1. Generate a Harness (or use an existing one)

```bash
tools/coding-harness/scripts/scaffold-context-harness.sh \
  --root ~/.agent-core/harnesses \
  my-test-harness
```

### 2. Run the generated tests

```bash
cd ~/.agent-core/harnesses/my-test-harness
npm test
```

### 3. Start the Harness server

```bash
cd ~/.agent-core/harnesses/my-test-harness
npm start
```

The server listens on `http://127.0.0.1:17400` by default.

### 4. Run the Smoke Runner

```bash
node tools/coding-harness/scripts/smoke-context-harness.mjs \
  --manifest ~/.agent-core/harnesses/my-test-harness/harness.manifest.json \
  --expect-fragment "EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya"
```

If all checks pass, the runner prints a text report that includes
`READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION` and exits with code `0`.

If any check fails, the report includes `Result: FAIL` with the failed
check name and reason, and exits with code `1`.

### 5. Interpret the report

On success, the report shows every check as `PASS`, the
`READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION` readiness marker, suggested
environment variables for manual registration, and rollback instructions.

On failure, the report shows `Result: FAIL`, the specific check that failed,
the reason, and a confirmation that no Kernel env was modified.

## Smoke runner arguments

| Argument | Required | Description |
|----------|----------|-------------|
| `--manifest <path>` | yes | Absolute or relative path to `harness.manifest.json` |
| `--expect-fragment <str>` | no | Expected substring in at least one fragment content |
| `--help` / `-h` | no | Show usage message |

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | All checks pass (`READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION`) |
| `1` | One or more checks failed |

## Reading the report

The report is plain text printed to stdout, structured in sections.

### Passing report

```
HARNESS_SMOKE_RUNNER_REPORT

Manifest:
- path: /path/to/harness.manifest.json
- harness_id: my-test-harness
- kind: context.prepare.v0

Checks:
- manifest schema: PASS
- local-only endpoint: PASS
- health URL local-only: PASS
- smoke command: PASS
- health: PASS
- context.prepare: PASS
- smoke word: PASS

Readiness:
- READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION

Suggested manual env:
- AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true
- AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=http://127.0.0.1:17400/context.prepare.v0
- AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open
- AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS=1000

Rollback:
- restore previous AGENT_CORE_CONTEXT_PREPARE_HOOK_URL
- or set AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=false

Safety:
- No Kernel env was modified.
- No hook was enabled.
```

### Failing report

```
HARNESS_SMOKE_RUNNER_REPORT
Result: FAIL

Manifest:
- path: /path/to/harness.manifest.json
- harness_id: my-test-harness
- kind: context.prepare.v0

Checks:
- manifest schema: PASS
- local-only endpoint: FAIL
    host must be 127.0.0.1, localhost, or ::1, got "0.0.0.0"
- health URL local-only: PASS
- smoke command: PASS
- health: FAIL
    SMOKE_FAILED_HEALTH_UNREACHABLE: connect ECONNREFUSED ...
- context.prepare: FAIL
    request failed: connect ECONNREFUSED ...

Failed check: local-only endpoint
Reason: host must be 127.0.0.1, localhost, or ::1, got "0.0.0.0"

No Kernel env was modified.
No hook was enabled.
```

The disclaimers at the bottom are always present, confirming the tool
performed no system modifications.

## Manual registration (after smoke pass)

If all checks pass and you want to register the Harness with the Kernel:

1. Follow the steps in [harness-manual-registration-smoke.md](./harness-manual-registration-smoke.md)
2. Set the environment variables shown in the report's
   `Suggested manual env` section
3. Restart the Kernel
4. Verify the Kernel log shows the hook is enabled

`READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION` only indicates that the local
smoke checks passed. The Harness is **not** registered, enabled, or
connected to the Kernel until you complete the manual registration steps.

## Rollback

To disable a registered Harness:

1. Unset `AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED` or set it to `false`
2. Unset or restore `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL`
3. Restart the Kernel
4. Stop the Harness server process

See the [manual registration doc](./harness-manual-registration-smoke.md#rollback)
for detailed rollback strategies.

## Troubleshooting

### Smoke command fails

- Run `npm test` manually in the harness directory
- Check that `package.json` and test files exist
- Ensure the test server port is available

### Health check fails

- Verify the Harness server is running: `ps aux | grep server.mjs`
- Check the port: `curl http://127.0.0.1:17400/health`
- If the port is different, verify `health.url` in the manifest
- Check for port conflicts: `lsof -i :17400`

### Context.prepare check fails

- Verify the Harness server is running on the correct port
- Check the manifest's `endpoint.url` matches the server's listening address
- Send a test request manually:
  ```bash
  curl -s -X POST http://127.0.0.1:17400/context.prepare.v0 \
    -H 'Content-Type: application/json' \
    -d '{"hook":"context.prepare.v0","request_id":"debug-001","payload":{}}'
  ```

### Expected fragment not found

- Check the Harness server code for the fragment content
- Verify the `--expect-fragment` string matches exactly
- If the Harness is custom, check its `smoke.expected_observation` field

## Security notes

- Smoke Runner runs the manifest's `smoke.command` locally. Only run it on
  trusted Harnesses that you have created or reviewed.
- Smoke Runner makes HTTP requests to the addresses declared in the manifest.
  It checks that both the endpoint and health URLs are loopback-only.
- Smoke Runner never modifies env vars, files outside the manifest directory,
  or any Kernel configuration.
- Smoke Runner does not start, stop, or manage the Harness server process.
  The operator must manage the server lifecycle separately.

## Related documents

- [External Harness Scaffold v0](./external-harness-scaffold.md) — how to generate a Harness project
- [Harness Manifest v0](../architecture/harness-manifest-v0.md) — manifest schema reference
- [Harness Manual Registration](./harness-manual-registration-smoke.md) — runbook for manual registration

---

*End of document.*
