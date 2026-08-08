# execution-harness registration

Zero-Kernel registration of the generic execution primitives through the
Kernel's existing external-capability protocol.

## Prerequisites

- The execution-harness binary is running on a loopback address
  (e.g. `127.0.0.1:7650`) with `EXECUTION_HARNESS_TOKEN` set to the same
  value as the Kernel's `AGENT_CORE_CAPABILITY_HOST_EXECUTION_TOKEN`.
- The Kernel's `AGENT_CORE_IPC_TOKEN` is available for the two control
  calls.

## Register

`POST http://127.0.0.1:4130/v1/harness/register` with
`Authorization: Bearer <AGENT_CORE_IPC_TOKEN>`, one call per operation.

### external.coding_workspace_list

```json
{
  "harness_id": "execution-harness",
  "artifact_digest": "sha256:<binary sha256>",
  "protocol_version": "external-harness-v1",
  "endpoint": "http://127.0.0.1:7650/execute",
  "operation_name": "external.coding_workspace_list",
  "description": "List files in an isolated execution workspace.",
  "input_schema": {
    "type": "object",
    "properties": {"workspace_id": {"type": "string", "minLength": 1}},
    "required": ["workspace_id"],
    "additionalProperties": false
  },
  "output_schema": {
    "type": "object",
    "properties": {
      "workspace_id": {"type": "string"},
      "entry_count": {"type": "integer", "minimum": 0},
      "entries": {"type": "array"}
    },
    "required": ["workspace_id", "entry_count", "entries"]
  },
  "idempotent": true
}
```

### external.coding_workspace_read

```json
{
  "harness_id": "execution-harness",
  "artifact_digest": "sha256:<binary sha256>",
  "protocol_version": "external-harness-v1",
  "endpoint": "http://127.0.0.1:7650/execute",
  "operation_name": "external.coding_workspace_read",
  "description": "Read a file from an isolated execution workspace (path-fenced).",
  "input_schema": {
    "type": "object",
    "properties": {
      "workspace_id": {"type": "string", "minLength": 1},
      "relative_path": {"type": "string", "minLength": 1}
    },
    "required": ["workspace_id", "relative_path"],
    "additionalProperties": false
  },
  "output_schema": {
    "type": "object",
    "properties": {
      "workspace_id": {"type": "string"},
      "relative_path": {"type": "string"},
      "content": {"type": "string"},
      "bytes": {"type": "integer", "minimum": 0}
    },
    "required": ["workspace_id", "relative_path", "content", "bytes"]
  },
  "idempotent": true
}
```

### external.coding_workspace_write

```json
{
  "harness_id": "execution-harness",
  "artifact_digest": "sha256:<binary sha256>",
  "protocol_version": "external-harness-v1",
  "endpoint": "http://127.0.0.1:7650/execute",
  "operation_name": "external.coding_workspace_write",
  "description": "Write a file into an isolated execution workspace (path-fenced, size-capped).",
  "input_schema": {
    "type": "object",
    "properties": {
      "workspace_id": {"type": "string", "minLength": 1},
      "relative_path": {"type": "string", "minLength": 1},
      "content": {"type": "string"}
    },
    "required": ["workspace_id", "relative_path", "content"],
    "additionalProperties": false
  },
  "output_schema": {
    "type": "object",
    "properties": {
      "workspace_id": {"type": "string"},
      "relative_path": {"type": "string"},
      "ok": {"type": "boolean"}
    },
    "required": ["workspace_id", "relative_path", "ok"]
  },
  "idempotent": true
}
```

### external.coding_workspace_exec

```json
{
  "harness_id": "execution-harness",
  "artifact_digest": "sha256:<binary sha256>",
  "protocol_version": "external-harness-v1",
  "endpoint": "http://127.0.0.1:7650/execute",
  "operation_name": "external.coding_workspace_exec",
  "description": "Run a command in an isolated execution workspace with timeout and output caps (shell/git/build/test/local process/HTTP probe).",
  "input_schema": {
    "type": "object",
    "properties": {
      "workspace_id": {"type": "string", "minLength": 1},
      "command": {"type": "string", "minLength": 1},
      "args": {"type": "array", "items": {"type": "string"}},
      "relative_cwd": {"type": "string"},
      "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 120},
      "max_output_bytes": {"type": "integer", "minimum": 1024, "maximum": 65536},
      "shell": {"type": "boolean"}
    },
    "required": ["workspace_id", "command"],
    "additionalProperties": false
  },
  "output_schema": {
    "type": "object",
    "properties": {
      "workspace_id": {"type": "string"},
      "exit_code": {"type": "integer"},
      "stdout": {"type": "string"},
      "stderr": {"type": "string"},
      "timed_out": {"type": "boolean"},
      "stdout_bytes": {"type": "integer", "minimum": 0},
      "stderr_bytes": {"type": "integer", "minimum": 0},
      "stdout_truncated": {"type": "boolean"},
      "stderr_truncated": {"type": "boolean"}
    },
    "required": ["workspace_id", "exit_code", "stdout", "stderr", "timed_out"]
  },
  "idempotent": false
}
```

## Enable

`POST http://127.0.0.1:4130/v1/harness/enable` with
`Authorization: Bearer <AGENT_CORE_IPC_TOKEN>`:

```json
{
  "manifest_id": "<manifest_id returned by register>",
  "expected_snapshot_id": "<current active registry snapshot id>"
}
```

Register all four operations first, then enable them one by one; the
`expected_snapshot_id` for each enable is the active snapshot id returned
by the previous enable (or the current active snapshot id for the first).

## Verification

After enabling, the next run of the configured owner pins the new
snapshot; `provider_tools_for_grants` exposes the four operations, and the
generic external-harness adapter dispatches calls to
`http://127.0.0.1:7650/execute`.
