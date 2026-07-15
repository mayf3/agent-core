# Model Invocation Telemetry V0

Status: implemented. These Journal facts expose real model use to external
observers without adding a product-specific statistics operation to the Kernel.

## Boundary and source of truth

Every Runtime call to `LlmClient::complete` goes through one wrapper. The wrapper
writes a start fact before the provider call and one terminal fact after it:

```text
model.invocation.started.v0
  -> real LlmClient call
  -> model.invocation.completed.v0 | model.invocation.failed.v0
```

The Journal is authoritative. Consumers read the facts through
`event.observe.v0`; they do not read the Kernel SQLite database. The legacy
`LlmCompleted` event remains for compatibility and carries the model invocation
id, stable receipt id, and terminal receipt event id.

An invocation id is deterministic for a Run and model round:
`model:<run_id>:<round_index>`. Start and terminal writes use an immediate
transaction. Repeating the same start or terminal callback returns the first
durable fact, while attempting to change success into failure (or failure into
success) is rejected.

## Event contracts

`model.invocation.started.v0` contains:

- `schema_version`, `run_id`, `invocation_id`
- `profile`, `requested_provider`, `requested_model`
- `started_at`, `round_index`

`model.invocation.completed.v0` contains:

- `schema_version`, `run_id`, `invocation_id`, `receipt_id`
- `profile`, `provider`, `model`
- `started_at`, `finished_at`, `latency_ms`, `round_index`
- `input_tokens`, `cached_input_tokens`, `output_tokens`
- `reasoning_tokens`, `total_tokens`
- `finish_reason`, `error_category`, `estimated_cost`
- `provider_usage_extensions`

Unknown or unavailable usage and cost values are `null`. OpenAI-compatible
`prompt_tokens` and `completion_tokens` are normalized to `input_tokens` and
`output_tokens`. Cached and reasoning counters are accepted from the common
nested detail objects. A missing total is derived only when both input and
output counters are valid unsigned integers.

`model.invocation.failed.v0` contains the same identity, provider/model,
profile, timing, receipt, and round fields plus a fixed `error_category`. It
does not contain provider response bodies or raw errors.

## Data minimization

The telemetry payload is assembled from an explicit field list. It never selects
context blocks, user prompt text, response content, request headers, API keys, or
raw provider errors. Provider-specific usage extensions retain only bounded
numeric, boolean, null, array, and object data; strings and sensitive keys are
dropped.

`event.observe.v0` continues to redact credential-like keys recursively. The
five exact normalized token-counter keys may remain visible only when their value
is numeric or null. A string-valued counter and fields such as `access_token`
remain redacted.

## Consumer rule

A projection such as a Token Dashboard aggregates only
`model.invocation.completed.v0`. It advances and persists its own observe cursor,
treats replays as idempotent by event id or invocation id, preserves unknown
extension fields, and can rebuild its projection entirely from the Journal.

No `system.token_stats`, `system.cost_stats`, or `system.model_latency`
capability is introduced.
