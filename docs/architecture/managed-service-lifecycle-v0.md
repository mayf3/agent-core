# Managed Service Lifecycle v0

Managed services are external components. The Kernel records authority and
immutable facts; it never executes a candidate command, opens a listener for a
candidate, or passes Kernel/Feishu secrets to one.

## Trust and effect sequence

1. A catalogued `DevelopmentRequest` selects `hook-consumer-service-v0`.
2. Coding Harness materializes a candidate and the five acceptance gates bind
   its candidate, artifact, component manifest, and evidence digests.
3. Kernel derives a strict, content-addressed `deployment.service-manifest.v0`
   and creates an HCR-linked Proposal plus owner Approval.
4. An identity-bound owner decision revalidates Proposal, Approval, HCR,
   settlement, five gate attempts, receipt identity, origin Run, and all CAS
   objects.
5. Before any host effect, Kernel durably appends `deployment.intent.v0` and
   stores the exact `deployment.effect.v0` intent.
6. The loopback-only Deployment Harness loads the manifest and executable from
   CAS, starts the executable directly (never through a shell), supplies only
   the observer credential and managed-service environment, and waits for the
   declared health check.
7. Kernel accepts only an exactly bound healthy receipt. One transaction stores
   the receipt, creates an immutable Component Registry Snapshot, advances the
   component-state CAS, settles Approval and Proposal, and appends
   `deployment.receipt.v0`, `component.registered.v0`, and the activation fact.

Transport failures and malformed/5xx responses leave the Approval pending for
an identity-bound replay. A valid 4xx/409/422 rejection is terminal activation
failure. A managed service version must increase monotonically; rollback is a
separate explicit Harness action against a retained digest.

## Service manifest boundary

The manifest contains only:

- `component_id`, `kind`, artifact digest, fixed `entrypoint=artifact`, runtime
  profile, semantic version;
- required contracts and requested permissions;
- loopback/dynamic-port listen policy and bounded HTTP health check;
- relative state path and fixed upgrade/rollback policies.

Host paths, command arguments, environment values, tokens, arbitrary ports,
and shell fragments are not representable. v0 permits only
`event.observe.v0` plus `journal.observe` for `HookConsumerService`.

## Observer isolation

`AGENT_CORE_EVENT_OBSERVE_TOKEN` is accepted only by `/v1/events`. It must be
distinct from IPC, proposal-submit, and decision tokens. Deployment Harness
receives this read-only token but never receives the Kernel SQLite path, IPC
token, Feishu credentials, model credentials, or approval token. A service
keeps its own cursor and rebuildable projection below its managed state path.

## Component Registry

Long-lived components are not represented as fake operations. The separate
Component Registry stores immutable snapshots of component id, kind, manifest
and artifact identity, version, endpoint, deployment/receipt identity, status,
contracts, and permissions. Every install or upgrade publishes a new snapshot;
prior snapshots and receipts remain queryable for audit and rollback evidence.

Read-only component lookup accepts the proposal submit or decision credential
at `GET /v1/components/{component_id}`. Disable and rollback are not direct
Harness calls by operators: the Approval Workflow calls the Kernel's
`POST /v1/components/{component_id}/disable|rollback` route with the decision
credential, owner principal, nonce, expected component snapshot, and expected
deployment. Kernel records `component.control.intent.v0` before the effect,
validates the exact Harness receipt, and publishes the status change through a
component-snapshot CAS transaction. Rollback is restricted to the previous
artifact named by the active deployment receipt; an arbitrary historical
artifact cannot be selected. A pending deployment or control effect owns the
component until its replay settles, and an already settled control decision is
returned from the Journal before the Harness can be contacted again. This
prevents delayed rollback deliveries from mutating a later component version.
