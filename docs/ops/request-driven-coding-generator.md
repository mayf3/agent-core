# Request-driven Coding Generator

The Coding Harness generates `hook-consumer-service-v0` candidates from a
catalogued `DevelopmentRequest`. It does not contain a prebuilt Dashboard or
other product implementation: only the generic runtime, source policy,
request-to-model adapter, and trusted profile contract are fixed.

## Runtime configuration

The generator uses these optional Harness-process variables, falling back to
the Kernel-compatible model variables when an override is absent:

| Generator variable | Fallback |
| --- | --- |
| `CODING_GENERATOR_BASE_URL` | `AGENT_CORE_OPENAI_BASE_URL` |
| `CODING_GENERATOR_API_KEY` | `AGENT_CORE_OPENAI_API_KEY` |
| `CODING_GENERATOR_MODEL` | `AGENT_CORE_MODEL` |
| `CODING_GENERATOR_TIMEOUT_SECONDS` | `75` (bounded to 10–75) |

The service account should receive only the model variables, Harness artifact
root, bind address/token, and sandbox/toolchain configuration it needs. Do not
load Feishu, Kernel IPC, Approval, Deployment control, or observer credentials
into the Coding Harness process.

The Kernel gives `external.coding_task_submit` a bounded fifteen-minute transport
window. A rejected initial response may be discarded and retried twice without
returning policy diagnostics to the model. A source that passes source policy
may then receive three diagnostic-guided repairs, or four when the initial
stage used fewer than all three attempts. Model calls are individually capped
at 75 seconds, compile probes at 60 seconds, and contract probes at 15 seconds.
An invalid or policy-rejected repair response may also be discarded and retried
without returning its rejection diagnostics to the model; every such retry
consumes the same six-call shared budget. The worst case is six model calls plus
five probe cycles (825 seconds), inside the outer envelope. Other external
operations keep the normal Harness timeout.

## Verification

Normal CI is credential-free:

```bash
cargo test --manifest-path tools/coding-harness/Cargo.toml
```

An operator-controlled, non-deploying real-model smoke is opt-in:

```bash
CODING_GENERATOR_BASE_URL=... \
CODING_GENERATOR_API_KEY=... \
CODING_GENERATOR_MODEL=... \
cargo test --manifest-path tools/coding-harness/Cargo.toml \
  --test generic_generator_live -- --ignored --nocapture
```

On Linux, candidate compile and contract probes require the normal HCR sandbox.
Sandbox absence or cleanup uncertainty fails closed as infrastructure failure.
The macOS host-compile escape hatch exists only in debug builds for the local
real-model smoke; release builds cannot enable it and it is not a production
configuration.

Successful generation is still only a candidate. The five profile gates,
Proposal, owner Approval, Deployment Intent, Deployment Harness receipt,
healthcheck, and Component Registry publication remain mandatory.
