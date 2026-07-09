# Harness Workspace Bootstrap

> **Audience**: Agent Core developers and operators
> **Prerequisite**: Coding Harness (`tools/coding-harness`) is built and running

## Overview

The **External Harness Workspace** is the designated directory for developing,
testing, and running user-private or rapidly evolving Harnesses.

```
~/.agent-core/harnesses/
```

This directory is **not** part of the Agent Core main repository. It is an
external workspace that the Coding Harness can operate on when configured
with the `harness-dev` workspace ID.

## Convention

| Aspect | Detail |
|--------|--------|
| **Default path** | `~/.agent-core/harnesses/<harness-name>/` |
| **Workspace ID** | `harness-dev` |
| **Owner** | User / Agent (via Coding Harness) |
| **Kernel knowledge** | None — Kernel does not read or manage this directory |
| **Lifecycle** | Managed by the user or agent through Coding Harness operations |

### What belongs here

- User-private tools and scripts
- Project memory readers
- Business API wrappers
- Rapid experimentation harnesses
- Harnesses created or modified by an agent

### What does NOT belong here

- Reference harnesses — these live in `tools/<harness>/` in the main repo
- Kernel code
- DB state or journal data
- Secrets or credentials (see security notes below)

## Setup

### Step 1: Create the directory

```bash
mkdir -p ~/.agent-core/harnesses
```

### Step 2: Configure `CODING_CONFIG`

Set the `CODING_CONFIG` environment variable to include a `harness-dev`
workspace entry:

```bash
export CODING_CONFIG='{
  "workspaces": {
    "harness-dev": {
      "root": "'"$HOME"'/.agent-core/harnesses",
      "read": true,
      "write": true,
      "exec": true,
      "opencode": true,
      "network": true,
      "shell": false
    }
  }
}'
```

Or use the provided script:

```bash
source tools/coding-harness/scripts/init-harness-workspace.sh
```

### Step 3: Restart the Coding Harness

```bash
cargo run --bin coding-harness -- --listen 127.0.0.1:7200
```

Verify on startup:

```
[coding-harness] loaded 1 workspace(s)
[coding-harness] workspace 'harness-dev' → /Users/<you>/.agent-core/harnesses
```

## Workspace permissions

| Permission | Value | Scope |
|------------|-------|-------|
| `read` | `true` | List files, read file contents |
| `write` | `true` | Create and modify files |
| `exec` | `true` | Run commands inside the workspace |
| `opencode` | `true` | Submit OpenCode tasks (agent-driven development) |
| `network` | `true` | Allow network access for task execution |
| `shell` | `false` | **Disabled** — raw shell access is not granted |

These permissions are suitable for **owner-only / development scenarios**
where an agent is creating, testing, and iterating on Harnesses.

## Security boundary

The Coding Harness enforces these protections on all workspace operations:

### Path traversal

```
User-provided "relative_path" values are validated before any operation:
  - Absolute paths (/etc/passwd)        → rejected
  - Parent traversal (../../../etc)     → rejected
  - Outside workspace boundary          → rejected (via canonicalize + starts_with)
```

### Symlink escape

Symlinks inside the workspace that point outside the workspace root are
detected and rejected during write operations and capability proposals.
Read/list operations implicitly reject escapes through path canonicalisation.

### Loopback-only endpoint

The Coding Harness binds to `127.0.0.1` by default. The Kernel's external
harness transport also validates that endpoints are loopback addresses only.

### No automatic Kernel enablement

Configuring the Coding Harness with a `harness-dev` workspace does **not**:
- Register any hooks with the Kernel
- Modify `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL`
- Start any external Harness services
- Restart the Kernel
- Expose coding tools to all Feishu users

Hook registration remains a manual, explicit step (set env var, restart
Kernel). Production hook enablement requires approval in a future phase.

## Using the workspace

### Via Coding Harness protocol

Once configured, any tool that communicates with the Coding Harness can
use workspace ID `harness-dev`:

```json
{
  "operation": "external.coding_workspace_write",
  "arguments": {
    "workspace_id": "harness-dev",
    "relative_path": "my-harness/server.ts",
    "content": "// ...",
    "mode": "replace"
  }
}
```

```json
{
  "operation": "external.coding_workspace_exec",
  "arguments": {
    "workspace_id": "harness-dev",
    "command": "node",
    "args": ["server.ts"],
    "relative_cwd": "my-harness",
    "timeout_seconds": 30
  }
}
```

### Via direct filesystem access

Since `~/.agent-core/harnesses/` is a regular directory, you can also
interact with it directly:

```bash
ls ~/.agent-core/harnesses/
cd ~/.agent-core/harnesses/
```

## Example: creating a simple Harness workspace

```bash
# Create a harness directory and an entry point.
mkdir -p ~/.agent-core/harnesses/hello-harness
cat > ~/.agent-core/harnesses/hello-harness/manifest.json <<'EOF'
{
  "schema_version": "harness-manifest-v0",
  "harness_id": "hello-harness",
  "kind": "context.prepare.v0",
  "entrypoint": "node server.ts",
  "endpoint_url": "http://127.0.0.1:17401/context.prepare.v0",
  "health_url": "http://127.0.0.1:17401/health"
}
EOF
```

## Verification

To verify the workspace is operational:

1. Start the Coding Harness with the `harness-dev` workspace configured.
2. Run a workspace list:

```bash
curl -s -X POST http://127.0.0.1:7200/execute \
  -H 'Content-Type: application/json' \
  -d '{
    "protocol_version": "external-harness-v1",
    "operation": "external.coding_workspace_list",
    "arguments": {
      "workspace_id": "harness-dev",
      "relative_path": "."
    }
  }' | jq .
```

3. Run a workspace exec:

```bash
curl -s -X POST http://127.0.0.1:7200/execute \
  -H 'Content-Type: application/json' \
  -d '{
    "protocol_version": "external-harness-v1",
    "operation": "external.coding_workspace_exec",
    "arguments": {
      "workspace_id": "harness-dev",
      "command": "echo",
      "args": ["harness-dev workspace is operational"],
      "relative_cwd": ".",
      "timeout_seconds": 10
    }
  }' | jq .
```

## Relationship to reference harnesses

| Path | Role | Versioned with Kernel |
|------|------|-----------------------|
| `tools/<harness>/` | Reference harness — stable, reviewed, part of repo | Yes |
| `~/.agent-core/harnesses/<name>/` | User/agent Harness — private or rapidly evolving | No |

Reference harnesses demonstrate the hook ABI and serve as smoke test
targets. External Harness workspaces are for development, experimentation,
and agent-driven iteration. A mature Harness may later be promoted to
a reference harness or published as a standalone repository.

## Next steps

After bootstrapping the workspace, the next milestones are:

1. **Create a Harness** — use the Coding Harness to develop a custom
   hook server in `~/.agent-core/harnesses/<name>/`.
2. **Register hook** — set `AGENT_CORE_CONTEXT_PREPARE_HOOK_URL` to
   point at the new Harness endpoint.
3. **Smoke test** — run the context-harness smoke checks against the
   new Harness.
4. **Iterate** — modify, test, and redeploy the Harness via the
   Coding Harness.

---

*End of document.*
