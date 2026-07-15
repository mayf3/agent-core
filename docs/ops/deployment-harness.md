# Deployment Harness Operations

Build and test:

```bash
cargo test --manifest-path tools/deployment-harness/Cargo.toml
cargo build --release --manifest-path tools/deployment-harness/Cargo.toml
```

Required environment:

```text
DEPLOYMENT_HARNESS_LISTEN_ADDR=127.0.0.1:7400
DEPLOYMENT_HARNESS_ARTIFACT_ROOT=<same CAS root used by Coding Harness and Kernel>
DEPLOYMENT_HARNESS_STATE_ROOT=<dedicated deployment state directory>
DEPLOYMENT_HARNESS_CONTROL_TOKEN=<random 32+ character control credential>
DEPLOYMENT_HARNESS_EVENT_OBSERVE_URL=http://127.0.0.1:4130/v1/events
DEPLOYMENT_HARNESS_EVENT_OBSERVE_TOKEN=<random 32+ character observer credential>
```

Kernel must use the same control credential through
`AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_TOKEN`, and its control URL defaults to
`http://127.0.0.1:7400`. Kernel must expose the same observer credential as
`AGENT_CORE_EVENT_OBSERVE_TOKEN`. The control and observer credentials must be
different.

Run:

```bash
tools/deployment-harness/target/release/deployment-harness
```

Only `/health` is unauthenticated. Deployment, status, disable, and rollback
routes require the control bearer. The listener and event observer URL must
resolve exclusively to loopback addresses.

The state root contains versioned installed executables, component-local state,
logs, immutable deployment records, and one active record per component. Back
up this root together with Kernel Journal backups. A rollout is healthy only
after the Harness `/health`, Kernel `/health`, active component status, service
health endpoint, and Journal deployment receipt all agree.

Rollback uses the retained previous artifact and starts it to readiness before
stopping the current process. Disable stops the managed process and records the
disabled state. Both operations persist the last typed control receipt in the
active record, so retrying the same Kernel decision after an uncertain response
is effect-idempotent. Production operators must invoke these effects through
the Kernel's owner-gated component control routes, not by calling the Harness
directly. Never point the artifact or state roots at a repository, Kernel data
directory, or Feishu Connector directory.
