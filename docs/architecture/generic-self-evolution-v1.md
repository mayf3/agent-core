# Generic Self-Evolution V1

Status: implemented foundation. This document defines the governed boundary used
to develop more than one kind of external component. It does not move product
features, deployment engines, dashboards, memory, schedulers, or repair loops
into the Kernel.

## Boundary

The Kernel records facts, binds identity and immutable digests, evaluates
proposals, validates receipts, and publishes registry snapshots. The external
Coding Harness discovers catalogued contracts and profiles, produces a candidate,
and runs profile gates. A candidate never becomes active merely because it was
built or tested.

The governed flow is:

```text
message -> DevelopmentRequest -> DevelopmentPlan -> candidate
        -> five profile gates -> immutable manifest digest
        -> Proposal -> Approval -> activation or deployment
```

`DevelopmentRequest` binds the source subject, scope, message, target kind,
requirements, contracts, permissions, build and deployment profiles, acceptance
criteria, idempotency key, and Contract Catalog version. Its `request_id` is
derived deterministically from the canonical request.

The first target kinds are:

- `invocable_capability`
- `hook_consumer_service`
- `context_provider`
- `context_transformer`
- `scheduled_worker`
- `scheduler_service`
- `ingress_router`
- `multi_run_orchestrator`
- `connector_extension`

The first Component Profiles are `invocable-capability-v0`,
`hook-consumer-service-v0`, `context-provider-v0`, `context-transformer-v0`,
`scheduled-worker-v0`, `router-service-v0`, and
`multi-run-orchestrator-v0`. Each profile declares project shape, reproducible
build steps, the five gates, sandbox policy, dependencies, allowed contracts and
permissions, artifact manifest, deployment, healthcheck, and rollback behavior.

## Contract Catalog

`contract-catalog-v1` exposes schemas, permissions, examples, SDK bindings, test
kits, compatibility, lifecycle, and health semantics for:

- `event.observe.v0`
- `context.prepare.v0`
- `context.load.v0`
- `context.compress.v0`
- `route.proposal.v0`
- `run.create.v0`
- `component.invoke.v0`
- `deployment.effect.v0`
- `feishu.reply.v0`

The Router and Coding Harness both use this catalog. A request fails closed when
a contract is unknown, its required permission is absent, or its selected
profile does not allow the contract or permission.

## Candidate and acceptance binding

Candidates use `component-artifact-v1`. The generic gate dispatcher derives its
build entry and test kit from the manifest and selected profile. Gate code does
not choose a calculator command or calculator source directly.

On a passing result, the Coding Harness stores the canonical, post-gate component
manifest in content-addressed storage. Its digest is included in signed acceptance
evidence and returned to the Kernel. The Kernel validates that digest, loads that
exact manifest from content-addressed storage, and derives the proposal from it.
It never activates a pre-gate manifest supplied by the submit response.

The existing calculator is retained as the first ordinary fixture for the
`invocable-capability-v0` profile. It supplies its own trusted test and smoke
vectors through the fixture registry so the prior live path remains covered
without defining the generic protocol.

## Request-driven Hook Consumer generation

`hook-consumer-service-v0` is the first non-fixture implementation of the
generic path. The Coding Harness sends only the request specification (not its
principal, scope, source message, credentials, or host configuration) to an
OpenAI-compatible code model. The model may implement four pure projection and
rendering functions. A fixed Harness-owned runtime implements event polling,
cursor persistence, read-only HTTP, identity headers, health, and telemetry
metadata.

The generated mutable surface is exactly `src/component.rs`. Source policy
parses the module as Rust and rejects extra public APIs, host/network/process/
environment access, unsafe/FFI code, includes, custom macros, scripts, and
imports outside the supplied JSON/helpers/collection prelude. The candidate has
a frozen dependency lock and is compiled in an isolated, network-denied probe.
Compiler or profile-contract failures may drive at most four model repair
passes. A probe or cleanup infrastructure failure is never reclassified as a
candidate failure and never relaxes isolation.

Only after the isolated compile and profile contract pass does the Harness
atomically materialize a request-bound candidate. Its manifest records the
DevelopmentRequest id, model name, module digest, test kit, and mutable surface.
The ordinary five HCR gates still run afterward and bind the accepted artifact;
the generation probe is not a substitute for Proposal, Approval, deployment,
or receipt validation.

## Lifecycle, primitive gaps, and repair

The component lifecycle is governed as:

```text
planned -> candidate -> accepted -> proposed -> approved -> deploying -> healthy
```

Failure, disable, redeploy, and rollback transitions are explicit. Approval and
deployment are separate states; passing gates is not approval.

`PrimitiveGapProposal` is required only after attempted derivation through the
existing contracts proves that a raw fact or governance boundary is missing. It
records the minimal proposed primitive and a `resume_token` so the original work
can continue after the gap is resolved. Implementation convenience is not a
primitive gap.

`RepairRequest` classifies observed failures as `ComponentBug`,
`ContractMismatch`, `MissingPrimitive`, `InfrastructureFailure`,
`ConfigurationError`, `PermissionDenied`, `DataCorruption`, or
`DependencyUnavailable`, and binds evidence, reproduction, the current artifact
digest, requested fix, risk delta, and rollback target.

Long-running service installation uses the separate Deployment Harness and
`deployment.effect.v0`; the Kernel never runs caller-selected shell commands.
