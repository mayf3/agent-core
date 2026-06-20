# Granting `time.now` to the dogfood Agent

This document explains how to grant a read-only tool (e.g. `time.now`) to the
dogfood Agent **without** editing Kernel code or the Feishu channel baseline.

## Principle

Tool grants belong to the **Agent profile**, never to a channel. The Kernel
exposes a tool to the provider only when it is BOTH:

1. granted to the run principal (via the Agent's `ExecutionProfile`), AND
2. a catalogued `ReadOnly` operation.

The Gateway is always the final authorization boundary — even if the provider
fabricates a tool call for an un-granted operation, the Gateway rejects it with
a `ToolCallRejected(policy_denied)` fact and the capability never executes.

## How to grant `time.now`

Set `AGENT_CORE_EXTRA_ALLOWED_OPERATIONS` in the Kernel service's external
environment. Multiple operations are comma-separated:

```text
AGENT_CORE_EXTRA_ALLOWED_OPERATIONS=time.now
```

If you already grant other operations (e.g. the dogfood `system.status`), list
them together:

```text
AGENT_CORE_EXTRA_ALLOWED_OPERATIONS=time.now,system.status
```

> Note: `system.status` is added to the dogfood profile automatically by the
> Kernel default config. The `time.now` grant is not defaulted; you must grant
> it explicitly through this external setting.

After this PR is merged, update the external service configuration and restart
the Kernel yourself. The provider request will then include the `time.now`
function schema, the model may emit a `time.now` tool call, and the Gateway will
approve it because it is explicitly granted. The Journal will show:

```text
ToolCallIssued
InvocationProposed(time.now)
InvocationApproved(time.now)
ReceiptReceived Succeeded
```

## What changed in the bootstrap Prompt

Old Phase-0 templates said "Keep Phase 0 chat-only" / "answers user messages
without tools", which suppressed tool use even when tools were authorized. The
current templates describe generic capability boundaries and tell the model to
prefer an authorized read-only tool over guessing for real-time/system/session
facts. See `docs/bootstrap-prompt-migration.md` for the exact migration rules.

## No keyword routing

The Kernel never routes on keywords like "几点" / "time". The model decides when
to call a tool from the provided schemas; the Gateway decides whether it is
allowed.
