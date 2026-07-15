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
window. Model calls are individually capped at 75 seconds, compile probes at 60
seconds, and contract probes at 15 seconds; therefore the initial pass plus all
four permitted repairs remain inside that outer envelope. Other external
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
