# system-status Skill

The model may call `system.status` to read the Kernel's current health and
projection state. The capability returns structured JSON containing aggregate
journal counts and a rollup status string — never secrets, payloads, or raw
event content.

## When to invoke

When the user asks about any of the following (or similar phrasings):

- System status / health / state
- "现在状态怎么样" / "系统状态" / "健康状况"
- Outbox, queue, dispatcher, projection
- "为什么降级 / degraded / corrupt"
- Error categories, recovery, drift

## How to invoke

Call `system.status` with **no arguments**.

The tool is a `Risk::ReadOnly` catalog operation. The Gateway checks the
run's grant set; the default dogfood agent profile includes the grant.

## How to format the reply

Only use the structured fields returned by the tool. Never fabricate or
guess values that are not present in the result.

The returned JSON has the following fields:

| Field | Type | Meaning |
|-------|------|---------|
| `status` | string | `"ok"`, `"degraded"`, or `"corrupt"` (rollup) |
| `hash_chain_ok` | boolean | Journal hash-chain integrity |
| `event_count` | integer | Total journal events |
| `outbox.pending` | integer | Dispatches waiting to be sent |
| `outbox.dispatching` | integer | Dispatches currently in flight |
| `outbox.unknown_unacked` | integer | Terminal-unknown dispatches not yet acknowledged |
| `outbox.stale_dispatching` | integer | Expired lease dispatches (self-healing) |
| `outbox.projection_drift` | integer | Projection/journal terminal-fact disagreements |
| `ingress.undelivered` | integer | Events accepted but not yet processed |
| `approval.awaiting` | integer | Runs paused for human approval |
| `summary` | string | One-line rollup for quick reference |

## Reply format

Prefer a concise, block-style reply:

```text
📊 系统状态：ok
hash chain: ✅ intact   journal events: 42
outbox pending: 0   unknown: 0
ingress undelivered: 0   drift: 0
approval awaiting: 0
```

If any non-zero value signals an anomaly, call it out explicitly:

```text
⚠ 系统状态：degraded
- projection drift: 3 (outbox 状态与 journal 终端事实不一致)
- unknown unacked: 1 (需人工确认的分发结果)
```

## Boundaries

- Return only the tool's output. Never admit to knowing any internal Kernel
  state beyond what `system.status` returns.
- Never return secrets, tokens, keys, or raw payload excerpts.
- Never output the full raw JSON — format into readable text.
- If the tool fails or returns an error, say the status tool is unavailable
  and suggest manual `/health` check.
