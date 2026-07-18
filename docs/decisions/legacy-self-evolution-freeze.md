# Legacy Self-Evolution Prototype Freeze

**Date:** 2026-07-18

## Status

Accepted. Scope: `refactor/external-orchestration-contraction-v0`.

## Context

The existing `agent-core-kernel` contains a working self-evolution / HCR
prototype that was built before the External Orchestration Seam architecture
was established. This prototype is in production use and must remain
operational during the transition. At the same time, the project must not
add new product features to this legacy path.

## Decision

The legacy self-evolution prototype is frozen under the following banners —
no code is deleted, but no new features, component types, or HCR gates are
added to it.

```text
LEGACY_SELF_EVOLUTION_PROTOTYPE
NO_NEW_FEATURES
NO_NEW_COMPONENT_TYPES
NO_NEW_HCR_GATES
```

Frozen areas (exact file-level scope the next migration milestone will
determine):
- `src/server/coding_router.rs` — the Chinese/English NL parser
- `src/server/coding_delivery.rs` — Feishu-p2p coding intent router
- `src/server/coding_task_submit.rs` — the end-to-end development pipeline
- `src/server/hcr_acceptance/` — the five-gate acceptance handler
- `src/domain/self_evolution.rs` — DevelopmentRequest, TargetKind, RepairRequest
- `src/domain/harness_change_request.rs` — GateKind (the five gates)
- `src/domain/coding_operations.rs` — the seven external.coding_* operation names
- `src/domain/capability_proposal_link.rs` — HCR → Proposal links
- `src/domain/service_manifest.rs` — hardcoded event.observe.v0 / .codes.v0
- `src/hcr/` — the HCR state machine
- `src/runtime/coding_grants.rs` — owner-based coding grant augmentation

Frozen tables (read only, no new writes after the new path takes over):
- `hcr_receipt_identities`
- `harness_change_requests` + child tables
- Gate attempt / evidence / settlement tables

The External Orchestration Seam V0 (`src/orchestration.rs`,
`crates/agent-core-protocol`, `tools/development-controller`) is the only
path for new orchestration capabilities. Any new development-related DTO or
endpoint must land in the seam, not in the legacy prototype.

## Consequences

- The legacy path remains fully operational as a fallback.
- No migration of existing data is required.
- The new seam is opt-in via `AGENT_CORE_EXTERNAL_ORCHESTRATION_URL`;
  production `.env` does not set this variable, so the legacy path is
  unaffected until the operator chooses to switch.
- A future milestone will delete the frozen code after all callers have
  been migrated to the seam.
