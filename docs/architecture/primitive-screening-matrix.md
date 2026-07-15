# Primitive Screening Matrix

> **Draft / Candidate Model — Not an immediate refactor plan.**
>
> This matrix screens every first-class concept in Agent Core against the
> candidate 8 primitives defined in
> [`kernel-primitive-calculus.md`](./kernel-primitive-calculus.md). Every row is
> backed by **real code evidence** (file:line), not by name similarity. The
> `Current decision` column says what to do *now*; the `Classification` column
> says what the concept *would be* in the candidate model. **No row in this
> matrix authorizes a deletion, migration, or API change in this round.**

## How to read this matrix

### Classification legend

```text
K = candidate irreducible Kernel primitive (in the candidate model)
D = derivable domain alias (expressible as a composition of the 8)
E = should-externalize strategy / product capability
S = phase scaffold / North Star scaffolding (present or stubbed)
U = under-evidenced; hold judgement
P = provisional / disputed primitive candidate (K8 Allow Boundary; see K8 section)
```

> **K8 caveat:** Allow Boundary (K8) is **provisional / disputed**, not a proven
> irreducible primitive — see the "K8 Allow Boundary — provisional / disputed
> screening" section. The candidate set is therefore "8 (provisional)" and is
> not asserted to be minimal.

### Decision legend

```text
Keep                : concept stays as-is, no action
Document            : keep, and document its derivation (this matrix does that)
Externalize later   : candidate to move outside the Kernel in a future round
Revisit             : re-screen when a stated trigger occurs
```

### Tally

```text
Candidate K: 8   (Identity, Scope, Snapshot, Run, Intent+Decision,
                   Journal Event, Receipt, Allow Boundary)
                  NOTE: K8 Allow Boundary is PROVISIONAL / disputed.
                  It is counted here so the screening has a label, but it
                  may demote to an inference rule (see "K8 re-screening"
                  below). The set is therefore 8 (provisional), and is NOT
                  claimed to be proven minimal and irreducible.
Candidate D: 15  (Agent, Principal, Session, Registry, HCR, Settlement,
                   Capability Proposal, Approval, Decision, InvocationIntent,
                   Invocation, Hook, ContextBlock, Capability, **Attempt**)
Candidate E: 6   (Adapter, Connector, Router, Scheduler, External Operation,
                   Workspace/Profile)
Candidate S: 3   (Registry Snapshot folds into K3; spawn stub; yield stub)
Candidate U: 1   (**Grant Revocation timing semantics** — §14 A/B/C dispute;
                  see new row 31)
Non-primitive layers: 2  (**Liveness** = temporal property L7 (row 29);
                          **Time/Clock** = environmental observation L8 (row 30))
```

> **Formula correction (sufficiency round):** rows 2 (Principal), 25
> (Capability), 26 (External Operation), and 27 (Workspace/Profile) previously
> stated grants are "derived from Snapshot(K3)". That is **wrong** — grants are
> independent, mutable authorization state (`external_operation_grants`), and
> `provider_tools_for_grants` **intersects** caller-supplied grants with
> snapshot definitions. See calculus §14 for the full correction and the
> deferred-revocation finding. No code was changed.

Note: tallies count *roles*. Some concepts carry more than one role (e.g.
Registry Snapshot is K3 *and* the Snapshot implementation); the per-row table is
authoritative, the tally is a summary.

---

## K8 Allow Boundary — provisional / disputed screening

K8 is **not** asserted to be an independently irreducible primitive. It is
retained as a candidate label so the screening in `kernel-primitive-calculus.md`
§3/§6 has a name for the enforcement surface, but its status is open. This
section records the explicit re-screening condition.

| Field | Content |
|---|---|
| Current concept | The non-bypassable enforcement point that gates Intent → Effect: `Risk::Write` forces the full intent → approval → adapter → receipt chain (`src/domain/operation.rs:90` `is_allowed`); `RegistrySnapshot::provider_tools_for_grants` is the model-visible catalog surface |
| Classification | **K8 — PROVISIONAL** (disputed; may demote to an inference rule) |
| Provisional reason | K5 already carries the authorization half (Decision) plus the gating check `is_allowed`. Much of what §3 groups under K8 is the *enforcement of the K5 transition*, not a separable object. K8 may not be a standalone object primitive at all — it may be an unbypassable execution rule / safety invariant enforced along Intent → Decision → Invocation. |
| Re-screening condition | **If every Allow Boundary semantic can be expressed as a transition invariant over Intent(K5) + Decision(K5) + Invocation(§4 Effect path), then K8 is no longer an independent primitive and demotes to an inference rule.** |
| What would *keep* K8 independent | Discovery of an Allow Boundary property that cannot be stated as a K5 transition invariant — e.g. a grant-resolution or catalog-visibility rule that must be enforced *outside* the Intent→Decision→Invocation path and is itself security load-bearing and cross-cutting (§2 criteria). Absent that, K8 is the least-supported candidate. |
| Current decision | **Revisit / hold** — keep the K8 label for screening continuity; do **not** assert the 8 primitives are proven minimal. No production change either way (this round changes no code). |

This provisional status is mirrored in `kernel-primitive-calculus.md` §3 (K8
note) and §10. It means the candidate set is described as "8 (provisional)" and
is **not** claimed to be proven minimal and irreducible.

---

## Matrix

### 1. Agent

| Field | Content |
|---|---|
| Current concept | A loaded agent config/profile (display name, skills, default model, profile path) |
| Code evidence | `struct Agent` at `src/domain/mod.rs:44-52`; `AgentId` newtype `src/domain/mod.rs:37`; `AgentStatus` (`Active`/`Disabled`) `src/domain/mod.rs:60-64`; `SkillRef` referenced from profile |
| Current owner | Kernel (domain type) + Harness (profile file on disk) |
| Durable state | **No dedicated table.** Only the `agent_id` column stamped on `sessions` and `runs` (`migrations/0001_init.sql:3,16`). Identity sourced from `KernelConfig.agent_id`, threaded into `SessionTarget` (`src/gateway/mod.rs:250`) |
| Security invariant | `agent_id` is part of Session/Run/RunPrincipal identity; default-deny across Agents (`docs/decisions/agent-home-directory-isolation.md` rules 5, 8) |
| Classification | **D** — derivable as Identity(K1) + Scope-bag(K2) + Snapshot-of-profile(K3) + Run(K4) |
| Candidate derivation | `Agent ≈ Identity(K1) + Scope(K2) + Snapshot(K3) + Run(K4)` |
| External contracts | `agent_id` string appears in Session/Run/Principal; profile files under `agents/main/AGENT.md` |
| Migration risk | Medium — `agent_id` is a stable foreign key; changing its representation ripples into sessions/runs/principal_json |
| Current decision | **Document** (keep; do not migrate to a Profile type now) |
| Trigger to revisit | Multi-profile / multi-Agent runtime becomes a real, repeated demand |

### 2. Principal

| Field | Content |
|---|---|
| Current concept | The calling identity of a Run: subject, source channel, grants, requester |
| Code evidence | `struct RunPrincipal` at `src/domain/mod.rs:91-98`; `PrincipalId` `src/domain/mod.rs:42`; `PrincipalSubject` (`LocalUser`/`FeishuOpenId`) `src/domain/mod.rs:100-104`; `PrincipalSource` (`Cli`/`Feishu`) `src/domain/mod.rs:106-110`; CLI path hardcodes `cli:local` (`src/gateway/mod.rs:352-362`), Feishu path builds `feishu:open_id:{...}` (`src/gateway/mod.rs:238-248`) |
| Current owner | Kernel |
| Durable state | Serialized to JSON in `runs.principal_json` (`migrations/0001_init.sql:19`, `src/journal/sqlite.rs:209`); `principal_id` referenced in grant/HCR/proposal columns |
| Security invariant | Grants are scoped per-Run; Feishu must not inherit CLI tool permissions (`docs/architecture-rfc.md` §4) |
| Classification | **D** — Identity(K1) carrying a **mutable grant set pinned at Run start** (grants are independent authorization state in `external_operation_grants`, minted-against but NOT derived from Snapshot(K3); see calculus §14) |
| Candidate derivation | `Principal ≈ Identity(K1) + pinned grants ⊆ (external_operation_grants ∩ snapshot.operations)` |
| External contracts | `principal_id` string format (`cli:local`, `feishu:open_id:...`) is a stable convention |
| Migration risk | Medium — `principal_json` schema is read by recovery + dispatch |
| Current decision | **Document** |
| Trigger to revisit | A third channel (e.g. `api`) lands and proves the abstraction |

### 3. Session

| Field | Content |
|---|---|
| Current concept | A conversation scope keyed by (agent, channel, conversation_key) |
| Code evidence | `struct Session` at `src/domain/mod.rs:66-77`; `SessionId` `src/domain/mod.rs:38`; `SessionStatus` (`Active`/`Archived`) `src/domain/mod.rs:85-89`; `ChannelKind` (`Cli`/`Feishu`) `src/domain/mod.rs:79-83`; `UNIQUE(agent_id, channel, conversation_key)` (`migrations/0001_init.sql:1-12`); `get_or_create_session` (`src/journal/sqlite.rs:156-192`); conversation turns assembled from journal in `src/journal/conversation.rs:24` |
| Current owner | Kernel |
| Durable state | `sessions` table (`migrations/0001_init.sql:1-12`) |
| Security invariant | Session key uniqueness prevents cross-conversation collision; cross-Agent default-deny |
| Classification | **D** — Scope(K2) + ordered Journal Events(K6) / Runs(K4) |
| Candidate derivation | `Session ≈ Scope(K2) + ordered Events(K6)/Runs(K4)` |
| External contracts | Session id (`session_<uuid>`), conversation_key, summarization pointer |
| Migration risk | High — sessions table is referenced by every run and ingress |
| Current decision | **Document** (the screening question "is Session just Scope + ordered Events?" is answered *yes in the model*, but no migration is performed) |
| Trigger to revisit | Long-term memory / automatic compression demand forces a Session re-shape |

### 4. Run

| Field | Content |
|---|---|
| Current concept | A single auditable execution lifecycle for one session turn |
| Code evidence | `struct Run` at `src/domain/mod.rs:146-168`; `RunStatus` (`Running`/`WaitingDispatch`/`Completed`/`Failed`/`AwaitingApproval`/`Unknown`) `src/domain/mod.rs:170-192`; `RunMode` (`Default`/`Hcr{...}`) `src/domain/mod.rs:121-138`; `runs` table (`migrations/0001_init.sql:14-25`); dispatcher queue `worker_jobs` (`src/journal/queue.rs:14-28`); lease loop `src/journal/worker.rs:10-88` |
| Current owner | Kernel |
| Durable state | `runs` table + `worker_jobs` projection (`src/journal/queue.rs`) |
| Security invariant | A Run pins one immutable `registry_snapshot_id` for its lifetime; status transitions are journaled |
| Classification | **K4** — candidate irreducible primitive |
| Candidate derivation | Primitive; pinned to Snapshot(K3) under Identity(K1) in Scope(K2) |
| External contracts | Run id (`run_<uuid>`), RunStatus string values, RunMode JSON |
| Migration risk | High — central to every flow |
| Current decision | **Keep** |
| Trigger to revisit | (none — primitive candidate) |

### 5. Journal Event

| Field | Content |
|---|---|
| Current concept | Append-only, hash-chained, monotonic fact log — the single source of truth |
| Code evidence | `struct JournalEvent` at `src/domain/mod.rs:371-383`; `JournalEventKind` enum (`src/domain/mod.rs:404-536`, ~30 variants); `journal_events` table with `previous_hash`/`hash`/`sequence` (`migrations/0001_init.sql:27-38`); append `src/journal/sqlite.rs:74-136` + tx variant `src/journal/queue.rs:127-183`; `verify_hash_chain` `src/journal/sqlite.rs:300-318` |
| Current owner | Kernel |
| Durable state | `journal_events` table |
| Security invariant | Append-only + SHA-256 hash chain detects tampering; monotonic sequence |
| Classification | **K6** — candidate irreducible primitive |
| Candidate derivation | Primitive |
| External contracts | `JournalEventKind` variant strings, payload schemas, correlation_id |
| Migration risk | Critical — changing any event kind breaks replay/eval and audit |
| Current decision | **Keep** (the Non-Action List forbids changing any Journal event kind) |
| Trigger to revisit | (none — primitive candidate) |

### 6. InvocationIntent

| Field | Content |
|---|---|
| Current concept | The Kernel's intent to invoke one catalogued operation on behalf of a Run |
| Code evidence | `struct InvocationIntent` at `src/domain/mod.rs:258-265`; `InvocationId` `src/domain/mod.rs:41`; catalog gating `is_allowed` (`src/domain/operation.rs:90`); `Risk::Write` forces the full intent→approval→adapter→receipt chain; durable once approved via `outbox_dispatches` (`src/journal/queue.rs:36-54`) |
| Current owner | Kernel |
| Durable state | `InvocationProposed` journal fact; `outbox_dispatches` row once approved |
| Security invariant | Unknown/unregistered operations are rejected before adapter execution; catalog is the authority |
| Classification | **K5** (proposal half) — candidate irreducible primitive |
| Candidate derivation | Primitive (paired with Decision as K5) |
| External contracts | `invocation_<uuid>`, operation name, arguments JSON, idempotency_key |
| Migration risk | High — the Intent→Decision→Receipt chain is the safety spine |
| Current decision | **Keep** |
| Trigger to revisit | (none — primitive candidate) |

### 7. Invocation (approved execution record)

| Field | Content |
|---|---|
| Current concept | The approved, dispatchable form of an intent |
| Code evidence | `struct ApprovedInvocation` at `src/domain/mod.rs:267-284` (private `intent` field, `pub(crate)` constructor); `outbox_dispatches` row (`src/journal/queue.rs:36-54`); `LeasedOutboxDispatch` `src/domain/mod.rs:385-394`; `OutboxDispatchStatus` (`Pending`/`Leased`/`Dispatching`/`Succeeded`/`Failed`/`RetryableFailed`/`Unknown`/`Dead`) `src/domain/status.rs:42-53` |
| Current owner | Kernel |
| Durable state | `outbox_dispatches` table (dispatch state machine) |
| Security invariant | Only an `ApprovedInvocation` (Intent + Decision) enters the outbox |
| Classification | **D** — `ApprovedInvocation = Intent(K5) + Decision(K5)` → outbox dispatch |
| Candidate derivation | `Invocation ≈ Intent(K5) + Decision(K5)` |
| External contracts | dispatch_id, invocation_id (UNIQUE), decision_id |
| Migration risk | Medium — outbox is a projection, recoverable from journal |
| Current decision | **Document** |
| Trigger to revisit | A second dispatch transport (beyond HTTP-localhost) is needed |

### 8. Receipt

| Field | Content |
|---|---|
| Current concept | Terminal outcome of one invocation (status, external_ref, output, time) |
| Code evidence | `struct Receipt` at `src/domain/mod.rs:286-293`; `ReceiptStatus` (`Succeeded`/`Failed`/`Unknown`) `src/domain/mod.rs:295-300`; recorded by transitioning `outbox_dispatches.status` (`src/journal/outbox_queue.rs:121-195`) + appending sanitized `ReceiptReceived` fact (`outbox_queue.rs:147-159`, safe fields only); HCR receipt identity table `hcr_receipt_identities` (`migrations/0010_hcr_receipt_identity.sql:10-35`) |
| Current owner | Kernel |
| Durable state | `outbox_dispatches` terminal status + `ReceiptReceived` journal fact; HCR-specific `hcr_receipt_identities` |
| Security invariant | Receipt binds exactly one invocation; only safe fields are journaled (never raw connector output) |
| Classification | **K7** — candidate irreducible primitive |
| Candidate derivation | Primitive |
| External contracts | ReceiptStatus strings, external_ref, sanitized output_kind |
| Migration risk | High — receipt identity underpins idempotency |
| Current decision | **Keep** |
| Trigger to revisit | (none — primitive candidate) |

### 9. Decision

| Field | Content |
|---|---|
| Current concept | The authorization half of K5; a replay-safe, action-bound human decision on a trusted capability Proposal |
| Code evidence | No standalone `Decision` struct; `TrustedDecisionIdentity` at `src/journal/trusted_capability_activation.rs:17-29`; `TrustedDecisionResult` `:37-45`; HTTP body `TrustedDecisionBody` `src/server/capability_decision.rs:22-33`; deterministic `decision_id` from canonical digest `src/server/capability_decision.rs:184-219`; persisted as columns on `capability_change_approvals` (`migrations/0012_capability_change_approvals.sql:31-39`) with all-or-none CHECK (`:46-77`) and immutable-binding trigger (`:109-117`) |
| Current owner | Kernel |
| Durable state | `capability_change_approvals` decision columns; run-level decisions via `runs.status='AwaitingApproval'` + journal facts (`ApprovalRequested`/`Granted`/`Denied`/`Expired`, `src/domain/mod.rs:451-457`) |
| Security invariant | Decision is content-bound (digest); binding immutable after creation; terminal state consistency enforced |
| Classification | **K5** (authorization half) — candidate irreducible primitive |
| Candidate derivation | Primitive (paired with Intent) |
| External contracts | `decision_<digest>` id format; run-level approval resume API |
| Migration risk | Critical — moving Approval→Decision table is explicitly forbidden this round |
| Current decision | **Keep** (Approval is kept as the stable domain facade over Decision) |
| Trigger to revisit | (none — primitive candidate) |

### 10. Approval

| Field | Content |
|---|---|
| Current concept | Two mechanisms: (i) run-level durable approval gating for `Risk::Write`; (ii) capability-change approval binding a human decision to a Proposal |
| Code evidence | (i) `RunStatus::AwaitingApproval` `src/domain/mod.rs:183`; facts `ApprovalRequested`/`Granted`/`Denied`/`Expired` `src/domain/mod.rs:451-457`; written `src/runtime/mod.rs:126-143`; expiry `src/journal/approval.rs:36-59`; resume `Gateway::approve_run` `src/gateway/mod.rs:368`. (ii) `struct CapabilityApproval` `src/domain/capability_approval.rs:15-36`; `CapabilityApprovalStatus` `:6-13`; `ApprovalReplayIdentity` `:41`; `capability_change_approvals` table `migrations/0012_capability_change_approvals.sql:7-78` |
| Current owner | Kernel |
| Durable state | (i) `runs.status` + journal facts; (ii) `capability_change_approvals` table |
| Security invariant | Write operations cannot execute without approval; capability binding immutable; resume rechecks policy + identity |
| Classification | **D** — a Human Subject's Decision(K5) over an Intent(K5), with a stable domain facade |
| Candidate derivation | `Approval ≈ HumanDecision(K5) over Intent(K5)` |
| External contracts | `/v1/approve`, `/v1/deny` endpoints; approval_id, expires_at |
| Migration risk | Critical — approval semantics affect duplicate-reply + safety |
| Current decision | **Keep** (treat as stable facade; do not collapse into a Decision table) |
| Trigger to revisit | A third approval shape appears that does not fit either mechanism |

### 11. Registry

| Field | Content |
|---|---|
| Current concept | The set of operation definitions the Kernel recognizes; authoritative pointer is `registry_state.active_snapshot_id` |
| Code evidence | Standalone `struct Registry` at `src/registry/store.rs:12`; production path is `impl JournalStore` in `src/journal/registry_ops.rs:26` (`initialize_registry:40`, `activate_snapshot_transactional:203` CAS + journal, `create_registry_snapshot:314`); `registry_state` singleton (`migrations/0003_external_harness_hotload.sql:23-31`); control ops seeded by `ensure_coding_control_operations` (`src/journal/registry_control_upgrade.rs:8-9`) |
| Current owner | Kernel |
| Durable state | `registry_state` (active snapshot id + CAS version), `harness_manifests` |
| Security invariant | Snapshot activation is atomic (CAS + journal event); external systems cannot expand model-visible tools silently |
| Classification | **D** — Snapshot(K3) catalogue + Allow Boundary(K8) for tool visibility |
| Candidate derivation | `Registry ≈ Snapshot(K3) catalogue + Allow(K8)` |
| External contracts | snapshot_id, registry_state pointer |
| Migration risk | High — runs pin snapshot_id; activation is CAS-guarded |
| Current decision | **Keep** (generalizing Registry is forbidden this round) |
| Trigger to revisit | A non-snapshot registry shape is proven necessary |

### 12. Registry Snapshot

| Field | Content |
|---|---|
| Current concept | An immutable, content-addressed set of operation definitions |
| Code evidence | `struct RegistrySnapshot` at `src/registry/snapshot.rs:78`; `OperationSpec` `:30-41`; `BindingKind` (`Builtin`/`External`) `:21`; `Risk` (`ReadOnly`/`Write`) `:11`; `compute_snapshot_id` `:142`; `registry_snapshots` + `registry_snapshot_operations` tables (`migrations/0002_registry_snapshots.sql:5-25`); runs pin `registry_snapshot_id` (`:27`) |
| Current owner | Kernel |
| Durable state | `registry_snapshots` (snapshot_id = digest), `registry_snapshot_operations` |
| Security invariant | Content-addressed (SHA-256); immutable once written; run pins one for its lifetime so Context/Provider/Gateway read the same frozen set |
| Classification | **K3** — candidate irreducible primitive |
| Candidate derivation | Primitive |
| External contracts | snapshot_id (digest), operation parameter schemas |
| Migration risk | High — pinned by every run |
| Current decision | **Keep** |
| Trigger to revisit | (none — primitive candidate) |

### 13. InvocationIntent — *see row 6*

(Covered above as K5 proposal half.)

### 14. HCR (Harness Change Request)

| Field | Content |
|---|---|
| Current concept | A durable pending request to develop a harness capability, claimed atomically, bound to one Run, progressed through 5 gates, settled terminally |
| Code evidence | `struct HarnessChangeRequest` `src/domain/harness_change_request.rs:12`; `HcrClaim`/`ClaimId`/`HcrClaimStatus` `:48-89`; `GateKind` (`Scaffold`/`Build`/`TrustedTest`/`TrustedSmoke`/`Artifact`) `:100`, `all_required()` `:131`; `HcrGateAttempt` `:145`; `HcrGateEvidence` `:163`; worker `execute_hcr` `src/hcr/worker.rs:35`; tables `harness_change_requests` (`migrations/0007_harness_change_requests.sql:18`), `hcr_claims`+`hcr_run_bindings` (`migrations/0008_hcr_claims.sql:16`), `hcr_gate_attempts`/`hcr_gate_evidence`/`hcr_settlements` (`migrations/0009_hcr_evidence.sql`), `hcr_receipt_identities` (`migrations/0010`); ghost-parent triggers `migrations/0009_hcr_evidence.sql:55-88` |
| Current owner | Kernel |
| Durable state | 6 HCR tables; `RunMode::Hcr` on the bound run |
| Security invariant | One active claim per HCR; one run per claim; idempotent source dedup; ghost-parent triggers reject orphans even with FK off |
| Classification | **D** — a Propose(§4) for a development Run(K4) + 5 gate Receipts(K7) + Settlement Decision(K5) |
| Candidate derivation | `HCR ≈ Propose(dev Run K4) + 5×Receipt(K7) + Settlement Decision(K5)` |
| External contracts | request_id, claim_id, the 5 gate names, settlement result strings |
| Migration risk | Critical — generalizing HCR is explicitly forbidden this round |
| Current decision | **Keep** (treat as the safe domain facade for Development Run; do not generalize) |
| Trigger to revisit | A non-development workflow needs the same gate pattern (then: extract a shared contract, do not generalize HCR in place) |

### 15. Settlement

| Field | Content |
|---|---|
| Current concept | The single atomic terminal transaction per HCR |
| Code evidence | `struct HcrSettlement` `src/domain/harness_change_request.rs:189`; `SettlementResult` (`Succeeded`/`CandidateFailed`/`InfrastructureFailure`/`AlreadySettled`/`EvidenceIncomplete`/`EvidenceConflict`) `:202`; entry `settle_hcr` `src/hcr/settlement.rs:8`; `settle_hcr_in_tx` `src/journal/hcr_settlement.rs:201` (accepts only identity keys; rejects caller-supplied result/digest, comment `:197-200`); `hcr_settlements` table (`migrations/0009_hcr_evidence.sql:33-43`, `UNIQUE(hcr_id)`) |
| Current owner | Kernel |
| Durable state | `hcr_settlements` (one terminal row per HCR) + terminal journal event |
| Security invariant | All validation inside `BEGIN IMMEDIATE`; caller cannot supply result or digest; CAS on HCR status |
| Classification | **D** — terminal Decision(K5) reducing a set of Receipts(K7) + Journal Event(K6) |
| Candidate derivation | `Settlement ≈ Decision(K5) ∘ Receipts(K7) + Event(K6)` |
| External contracts | settlement_id, result strings, evidence_set_digest |
| Migration risk | High — settlement is the HCR terminal anchor |
| Current decision | **Keep** |
| Trigger to revisit | (none — stays inside HCR) |

### 16. Capability Proposal

| Field | Content |
|---|---|
| Current concept | The Kernel's authoritative record of a trusted, externally-developed harness addition |
| Code evidence | `struct CapabilityChangeProposal` `src/domain/capability_change.rs:16`; `ProposalStatus` (`PendingApproval`/`Approved`/`Rejected`/`Activated`/`ActivationFailed`/`Expired`) `:94`; `CapabilityProposalHcrLink` `src/domain/capability_proposal_link.rs:13`; trusted path `create_proposal_with_hcr_link` `src/journal/capability_proposal_hcr.rs:11` (re-validates every field from authoritative rows, `validate_caller_fields:261`, `SOURCE_REGISTRY_SNAPSHOT_CHANGED` guard `:130-137`); tables `capability_change_proposals` (`migrations/0004`), `capability_proposal_hcr_links` (`migrations/0011:10`, digest CHECK length=71), `capability_change_approvals` (`migrations/0012`) |
| Current owner | Kernel |
| Durable state | 3 proposal/approval/link tables; activation flows into registry_snapshots |
| Security invariant | Kernel re-hashes artifact/manifest/evidence (never trusts submitter digests); binding immutable; cross-checks HCR settlement + receipt identity |
| Classification | **D** — Propose(§4) carrying digests → Decision(K5) → Snapshot activation(K3) |
| Candidate derivation | `Capability Proposal ≈ Propose(digests) → Decision(K5) → Snapshot(K3)` |
| External contracts | proposal_id, artifact/manifest/evidence digests, requested_operations |
| Migration risk | High — trusted activation chain |
| Current decision | **Keep** |
| Trigger to revisit | A second trusted-proposal source (beyond HCR) appears |

### 17. Hook

| Field | Content |
|---|---|
| Current concept | Stateless Kernel→external-harness call bindings at fixed lifecycle points |
| Code evidence | `HookKind` enum (`IngressRouteV0`/`ContextPrepareV0`/`ContextLoadV0`/`ContextCompressV0`/`EventObserveV0`/`DecisionPolicyV0`) `src/hook/types.rs:19-49`; `HookConfig` `src/hook/config.rs:20-39`; `HookRegistryConfig` `:108-115`; `HookClient` trait `src/hook/client.rs:57-64` + `FakeHookClient`/`HttpHookClient`; consumed `src/runtime/hook_call.rs:37-49` (inserts `ContextBlockKind::HookFragment`); audited via `JournalEventKind::HookCallRecorded` (`src/domain/mod.rs:466`) |
| Current owner | Kernel (protocol) + Harness (implementation) |
| Durable state | **No hook table.** Config from env (`KernelConfig.context_prepare_hook`, `src/config.rs:98,185-194`); only `HookCallRecorded` journal facts |
| Security invariant | Hook is a Transform(§4) that must be re-validated by the final system guard (`docs/architecture-rfc.md` §5); HTTP client restricted to localhost |
| Classification | **D** — `Trigger × {Observe|Propose|Transform|Effect}(§4) × Contract × Component Binding` |
| Candidate derivation | `Hook ≈ Trigger × Mode(§4) × Contract × Binding` |
| External contracts | The 6 `*.v0` hook kind strings, request/response envelopes |
| Migration risk | Medium — hook ABI rename is forbidden this round; kind names are a contract |
| Current decision | **Keep** (conceptual lens only; do not rename HookKind) |
| Trigger to revisit | A 7th lifecycle point is proven necessary |

### 18. Adapter

| Field | Content |
|---|---|
| Current concept | Stateless transports that perform the concrete operation behind the Allow Boundary |
| Code evidence | `InvocationAdapter` trait `src/adapters/mod.rs:11-13`; `HttpConnectorAdapter` `:15-19` (impl `:31`, loopback-only `Endpoint::parse` rejects non-localhost `:191-193`); `StdoutAdapter` `:74`; external harness adapter `execute_external_harness_with_config` `src/adapters/external_harness.rs:56` |
| Current owner | Kernel (trait) + Harness (external impl) |
| Durable state | None — adapters are transports; results become `ReceiptReceived` facts |
| Security invariant | HTTP adapter hard-restricted to loopback; external harness carries IPC token |
| Classification | **E** — the Effect-side transport(§4) behind Allow Boundary(K8) |
| Candidate derivation | `Adapter ≈ Effect transport behind Allow(K8)` |
| External contracts | `ApprovedInvocation` → `Receipt` shape |
| Migration risk | Low — trait is small and stable |
| Current decision | **Externalize later** (the boundary is already correct; no action now) |
| Trigger to revisit | A non-HTTP, non-stdio transport is needed |

### 19. Connector

| Field | Content |
|---|---|
| Current concept | External processes bridging the Kernel to a chat platform (TypeScript Feishu connector) |
| Code evidence | **No Rust `Connector` type.** Kernel talks via `HttpConnectorAdapter` to `AGENT_CORE_CONNECTOR_EXECUTE_URL` (default `http://127.0.0.1:4131/v1/execute`, `src/config.rs:133-136`); TS connector under `connectors/feishu/` (`config.ts:5`, `kernel.ts:4` `postIngress`); only Rust token is `DispatchErrorCategory::ConnectorExecuteFailed` (`src/domain/mod.rs:331`) |
| Current owner | Harness (external process) |
| Durable state | None in Kernel SQLite; connector-local reaction state lives outside the checkout |
| Security invariant | Connector is an edge adapter; must not grow a second Session/Context/Policy/LLM/Gateway/Journal (`README.md`) |
| Classification | **E** — external process speaking Observe(§4) + Effect(§4) over IPC |
| Candidate derivation | `Connector ≈ external Observe+Effect over IPC` |
| External contracts | ingress + execute loopback HTTP contract, IPC token |
| Migration risk | Low (extraction target per `docs/connector-extraction-checklist.md`) |
| Current decision | **Externalize later** |
| Trigger to revisit | A second channel connector is built (forces contract extraction) |

### 20. ContextBlock

| Field | Content |
|---|---|
| Current concept | A transient, per-run LLM-context-window section assembled at runtime |
| Code evidence | `struct ContextBlock` `src/domain/context_block.rs:10-16`; `ContextBlockKind` (RootSystem/RuntimeContract/AgentProfile/SkillCatalog/ToolCatalog/ToolResult/ActiveSkill/RecentMessages/HookFragment/HarnessChangeRequest/UserMessage) `:18-34`; `Compressibility` `:36-42`; `ContextAssembler::build` `src/context.rs:24`; assembly from prompt files + journal turns + hook fragments (`src/context.rs:35-164`); `ContextBuilt` journal fact (`src/domain/mod.rs:409`) |
| Current owner | Kernel (assembler) + Harness (contributors) |
| Durable state | None — blocks are transient; only `ContextBuilt` fact recorded |
| Security invariant | Root System never overwritten/compressed by normal contributors (`docs/architecture-rfc.md` §7) |
| Classification | **D** — Transform-mode(§4) payload assembled per Run(K4) |
| Candidate derivation | `ContextBlock ≈ Transform(§4) payload per Run(K4)` |
| External contracts | `ContextBlockKind` variant names, compressibility levels |
| Migration risk | Medium — context pipeline invariants |
| Current decision | **Document** |
| Trigger to revisit | Token budget / compression becomes a real, repeated demand |

### 21. Router

| Field | Content |
|---|---|
| Current concept | Stateless, deterministic string/keyword matchers for North Star intents |
| Code evidence | `calculator_router::matches` `src/server/calculator_router.rs:9-14` (exact sentence match; doc "intentionally not a general expression parser"); `CodingIntent`/`CodingIntentKind` + `parse_coding_intent` `src/server/coding_router.rs:13-34` (doc "This is NOT an LLM router"); both drive HCR/coding_task submissions downstream |
| Current owner | Kernel (currently in-process, by design as North Star scaffolding) |
| Durable state | None — pure functions |
| Security invariant | Router only produces an intent; Kernel validates + creates the Run/Intent |
| Classification | **E** — external Propose(§4) component; Kernel only does schema validation + Identity + Policy + real Intent/Run creation |
| Candidate derivation | `Router ≈ external Propose(§4)` |
| External contracts | None yet (fixed matchers) |
| Migration risk | Low — externalizing router is forbidden to *implement* this round |
| Current decision | **Revisit** (conceptually external; keep in-process until a second routing rule proves the boundary) |
| Trigger to revisit | A third routing rule or an LLM-based router is needed |

### 22. Scheduler stub

| Field | Content |
|---|---|
| Current concept | **Not implemented.** A documented North Star only. |
| Code evidence | No struct/trait/migration. Doc references only: `docs/phase0-plan.md:375` (out of scope), `docs/product-roadmap.md:311` ("内置多 Agent scheduler" future), `docs/evolution-harness.md:106` (to be built), `docs/architecture-rfc.md:84` (`cron` as a hypothetical channel). `examples/time_harness.rs` is an external harness answering `external.time_now`, **not** a scheduler |
| Current owner | (none — North Star) |
| Durable state | None |
| Security invariant | N/A |
| Classification | **E** — external time logic → `run.create` Propose(§4); NOT a Kernel cron platform |
| Candidate derivation | `Scheduler ≈ external time logic → Propose(§4)` |
| External contracts | None |
| Migration risk | N/A |
| Current decision | **Revisit** (do not implement; if time-based runs are needed, build an external proposer) |
| Trigger to revisit | Scheduled briefing / recurring task becomes a real demand |

### 23. spawn stub

| Field | Content |
|---|---|
| Current concept | Explicitly disabled stub for cross-scope Run creation |
| Code evidence | `pub fn session_spawn() -> Result<()>` at `src/runtime/hook_call.rs:16-19` always `bail!("not_enabled:session.spawn")`; re-exported `src/runtime/mod.rs:88`; test `tests/m0_kernel.rs:218-222`; documented out-of-scope `docs/phase0-plan.md:288`; referenced `docs/decisions/agent-home-directory-isolation.md:43,65,143` |
| Current owner | Kernel (stub only) |
| Durable state | None |
| Security invariant | N/A (disabled) |
| Classification | **S** — future cross-Scope(K2) Run(K4) creation; currently `not_enabled` |
| Candidate derivation | `spawn ≈ future cross-Scope(K2) Run(K4)` |
| External contracts | The `not_enabled:session.spawn` error string |
| Migration risk | N/A — deleting the stub is forbidden this round |
| Current decision | **Keep** (do not delete the stub; it signals a reserved ABI) |
| Trigger to revisit | Multi-agent collaboration becomes a real demand |

### 24. yield stub

| Field | Content |
|---|---|
| Current concept | Explicitly disabled stub for Run control-suspend |
| Code evidence | `pub fn run_yield() -> Result<()>` at `src/runtime/hook_call.rs:21-24` always `bail!("not_enabled:run.yield")`; re-exported `src/runtime/mod.rs:88`; test `tests/m0_kernel.rs:223` |
| Current owner | Kernel (stub only) |
| Durable state | None |
| Security invariant | N/A (disabled) |
| Classification | **S** — future Run(K4) control-suspend; currently `not_enabled` |
| Candidate derivation | `yield ≈ future Run(K4) suspend` |
| External contracts | The `not_enabled:run.yield` error string |
| Migration risk | N/A — deleting the stub is forbidden this round |
| Current decision | **Keep** |
| Trigger to revisit | Long-running runs with interleaved waiting become a real demand |

### 25. Capability

| Field | Content |
|---|---|
| Current concept | Three meanings: (A) inline in-process tool functions; (B) durable capability-change proposals/approvals; (C) external Capability Host deployer |
| Code evidence | (A) `src/capabilities/mod.rs` + `system_status::execute` `src/capabilities/system_status.rs:24` (doc "Capabilities are not adapters — inline functions in the Runtime process"). (B) see rows 16 (Proposal) + 12 (Snapshot). (C) `CapabilityHostDeployer` trait + `HttpCapabilityHostClient` `src/server/capability_host_client.rs:33-52`; `CapabilityDeployRequest`/`Result` `:12-31` |
| Current owner | Kernel (all three) |
| Durable state | (A) none; (B) proposal/approval/link tables; (C) activation result in `capability_change_approvals` |
| Security invariant | Kernel re-hashes artifacts from `harness_artifact_root` (`src/config.rs:71-77`); never trusts submitter digests; `DefinitiveDeploymentRejection` marks non-retriable |
| Classification | **D** — Snapshot(K3) operation row + **per-principal grant state (independent, mutable)** |
| Candidate derivation | `Capability ≈ Snapshot(K3) op + grant state (external_operation_grants)` |
| External contracts | operation names, parameter schemas, deployment result |
| Migration risk | Medium |
| Current decision | **Document** |
| Trigger to revisit | Inline capabilities grow beyond read-only helpers |

### 26. External Operation

| Field | Content |
|---|---|
| Current concept | An operation with `BindingKind::External` in a Snapshot, authorized per-principal by a grant |
| Code evidence | `ExternalOperationGrant` `src/domain/operation.rs:311`; name constants `src/domain/coding_operations.rs:8-28` (`WORKSPACE_*`, `TASK_SUBMIT`, `HCR_ACCEPT`, `CAPABILITY_PROPOSE`); `coding_operation_risk` `:40` (unknown ops → `None` → safe Write fallback); `external_operation_grants` table (`migrations/0006_external_operation_grants.sql:22`, partial unique index `WHERE status='active'` `:44`); grant ops `src/journal/grant_ops.rs:99`; runtime augmentation `src/runtime/coding_grants.rs:38` (`augment_grants`, `is_coding_owner:15`, `hcr_allowed_operations:70`) |
| Current owner | Kernel |
| Durable state | `external_operation_grants` (append-only; revoked rows retained) |
| Security invariant | Non-coding/unknown external ops are never auto-granted; revocation is audit-retained (no row deletion) |
| Classification | **E** — Snapshot(K3) row with `BindingKind::External` + **mutable per-principal grant state** (`external_operation_grants`) |
| Candidate derivation | `External Operation ≈ Snapshot(K3) row + grant state (external_operation_grants)` |
| External contracts | operation name strings, grant_id, status strings |
| Migration risk | Medium |
| Current decision | **Document** |
| Trigger to revisit | A new external operation family outside coding is added |

### 27. Workspace / Profile configuration

| Field | Content |
|---|---|
| Current concept | **No first-class Workspace or Profile type.** Flat env-driven `KernelConfig` + in-memory `ExecutionProfile` + on-disk agent profile files |
| Code evidence | `KernelConfig` `src/config.rs:7-99` (~40 env fields, built each boot via `load_local_env` `:199-216`); `KernelConfig::from_cli` sets `workspace_dir = current_dir()` (`:104`, used only for legacy `.agent-core` path `:112-114`); `ExecutionProfile` `src/domain/operation.rs:209-211` with `for_channel:218` + `with_extra:257`; agent profile is `agents/main/AGENT.md` loaded by `ContextAssembler` (`src/context.rs:58-67`); `ContextBlockKind::AgentProfile` (`src/domain/context_block.rs:22`) is a context label, not a type |
| Current owner | Kernel (config) + Harness (profile files) |
| Durable state | None — config rebuilt each boot; profile is files on disk |
| Security invariant | Credentials are references, not values (`docs/decisions/agent-home-directory-isolation.md` rule 9) |
| Classification | **E** — Identity(K1) + on-disk profile + **mutable per-principal grant state** (no first-class type today) |
| Candidate derivation | `Workspace/Profile ≈ Identity(K1) + profile files + grant state (external_operation_grants)` |
| External contracts | env var names, `agent.toml`/`AGENT.md` conventions (future, per isolation decision) |
| Migration risk | Low (no type to migrate yet) |
| Current decision | **Revisit** (a first-class Profile type is a future multi-agent concern) |
| Trigger to revisit | Multi-profile collaboration / agent-home-directory isolation is implemented |

---

## Minimality Re-Screening (sufficiency round)

> Added in the sufficiency-and-state-semantics round. **This section only updates
> evidence; it does NOT crop the candidate set.** No primitive is added, merged,
> split, or removed. The judgments below use the criteria from calculus §2
> (non-derivable, security load-bearing, cross-cutting, stable shape) and
> explicitly **reject** "has/has-not a separate DB table" or "appears in the same
> call chain" as sufficient evidence of reducibility.

### K1 Identity vs K2 Scope — reducibility NOT PROVEN

| Question | Evidence |
|---|---|
| Do K1 and K2 carry distinct security responsibilities? | **Yes.** K1 (`PrincipalId`/`AgentId`/`RunId`/`InvocationId`/`EventId`, `src/domain/mod.rs:37-41`) is *forgeable-proof identity* — who a subject/agent/run is. K2 (`Session` keyed by `(agent_id, channel, conversation_key)`, `migrations/0001_init.sql:1-12`) is an *isolation namespace* — the conversation scope that enforces cross-conversation and cross-Agent default-deny (`docs/decisions/agent-home-directory-isolation.md`). |
| Is either derivable from the other? | **No.** A Scope references an `agent_id` (K1), but an Identity does not imply a unique Scope (one identity may have many sessions/scopes). Identity answers "who"; Scope answers "where is this isolated." These are orthogonal axes. |
| Are both cross-cutting? | **Yes.** Every Run, Intent, Journal event, and Receipt carries identity; every Run and Session is scoped. |
| Verdict | **K1/K2 reducibility: NOT PROVEN.** They remain distinct candidate primitives. |

### K4 Run vs K5 Intent/Decision vs K7 Receipt — reducibility NOT PROVEN

| Question | Evidence |
|---|---|
| Do K4, K5, K7 occupy distinct lifecycle roles? | **Yes.** K4 (`Run` + `RunStatus`, `src/domain/mod.rs:146-192`) is the *execution lifecycle* for one scope turn. K5 (`InvocationIntent` + `ApprovedInvocation`, `src/domain/mod.rs:258-284`; `is_allowed`/`evaluate_policy`) is the *authorization gate* — the No-Effect-without-Decision invariant. K7 (`Receipt` + `ReceiptStatus`, `src/domain/mod.rs:286-300`) is *terminal evidence* bound to exactly one invocation. |
| Is one derivable from another? | **No.** A Run may have zero or many Intents; an Intent requires a Decision but a Decision is not a Run; a Receipt is terminal and binds one invocation, not a Run. They compose (`Invocation ≈ Intent + Decision`, row 7) but are not substitutable. |
| Distinct stable contracts? | **Yes.** Run id (`run_<uuid>`), RunStatus values, decision id (`decision_<digest>`), invocation id, ReceiptStatus strings, and receipt identity (`hcr_receipt_identities` UNIQUE) are all independent external contracts. |
| Verdict | **K4/K5/K7 reducibility: NOT PROVEN.** They remain distinct candidate primitives. The judgment deliberately does **not** use "they appear in the same dispatch call chain" as reducibility evidence — co-occurrence in a call chain is explicitly rejected as a criterion. |

### K8 Allow Boundary — status unchanged (provisional/disputed)

The K8 re-screening condition (above) is **not satisfied** this round. The
§14 finding shows the grant-resolution rule is *run-start pinning of
`external_operation_grants`*, enforced via `evaluate_policy` reading the frozen
`run.principal.grants` (`src/gateway/policy.rs:66-101`) — i.e. it **is**
expressible as a transition invariant over Intent(K5) + Decision(K5) +
Invocation. That leans toward K8 demoting to an inference rule. However, the
§14 revocation-timing semantic (A/B/C) is still **UNRESOLVED**, and option C
(Hybrid: high-risk Effect revalidation) *would* be a grant-resolution rule
enforced outside the pure Intent→Decision→Invocation transition — which could
keep K8 independent. **Therefore K8 remains provisional/disputed; no decision
is forced this round.** The candidate set stays "8 (provisional)".

---

## New Rows (sufficiency round)

> These concepts were investigated this round but are **not** promoted to
> primitives. They are recorded as derived/internal/environmental so the matrix
> is complete.

### 28. Attempt (per-dispatch execution attempt)

| Field | Content |
|---|---|
| Current concept | The non-terminal execution lifecycle of one outbox dispatch or worker job: the `attempts` counter + the retry/backoff path |
| Code evidence | `attempts INTEGER` on `outbox_dispatches` (`src/journal/queue.rs:47`) and `worker_jobs` (`src/journal/queue.rs:21`); incremented on lease (`src/journal/outbox.rs:63`, `src/journal/worker.rs:47`); retry decision reads `attempts` vs `RetryPolicy.max_*_attempts` (`src/journal/outbox_queue.rs:415`, `src/journal/worker.rs:230`). **No general `Attempt` type**; only HCR has a first-class `HcrGateAttempt` (`src/domain/harness_change_request.rs:145`, `migrations/0009_hcr_evidence.sql:5-19`). |
| Current owner | Kernel (internal column) |
| Durable state | `attempts` column on dispatch/worker rows (derivable from the row's lifecycle) |
| Security invariant | None of its own — it is internal to reliable-effect delivery |
| Classification | **D** (derived object / internal implementation structure) |
| Candidate derivation | `Attempt ≈ non-terminal lifecycle of outbox/worker row (K7 substrate)` |
| External contracts | None (no stable external contract; HCR gate attempt is a domain-specific generalization) |
| Migration risk | N/A |
| Current decision | **Document** — do NOT promote to a primitive; no evidence it carries an independent security invariant |
| Trigger to revisit | A second domain beyond HCR needs first-class, separately-addressable attempts |

### 29. Liveness (reliable-effect delivery)

| Field | Content |
|---|---|
| Current concept | The temporal guarantee that approved effects are delivered at-least-once, retried with backoff, dead-lettered, and reconciled if Unknown |
| Code evidence | Two parallel state machines: `OutboxDispatchStatus` (`src/domain/status.rs:42-82`) and `WorkerJobStatus` (`src/domain/status.rs:3-40`); lease+reclaim (`src/journal/worker.rs:10-88`, `src/journal/outbox.rs:10-105`); retry/backoff (`src/domain/retry.rs:22-31`); dead-letter (`src/journal/outbox_queue.rs:458-498`); Unknown recovery (`src/journal/unknown.rs:75-146`) |
| Current owner | Kernel |
| Durable state | `outbox_dispatches`, `worker_jobs` (state + `attempts`/`available_at`/`locked_until`) |
| Security invariant | Terminal-transition guard (`TERMINAL_TRANSITION_ERROR`, `src/journal/outbox_queue.rs:8`); at-least-once delivery of approved effects |
| Classification | **Temporal property (L7)** — NOT a state-carrying primitive. See calculus §11/§12. |
| Candidate derivation | `Liveness ≈ temporal property over the trace (K6), enforced by the transition relation (L3)` |
| External contracts | OutboxDispatchStatus / WorkerJobStatus string values |
| Migration risk | N/A (no object to migrate) |
| Current decision | **Document** — liveness is a temporal property, not a primitive; no Liveness primitive added. Reliable delivery ≠ external Scheduler/Cron. |
| Trigger to revisit | A liveness requirement cannot be expressed as a transition invariant over existing state |

### 30. Time / Clock

| Field | Content |
|---|---|
| Current concept | Wall-clock reads (`Utc::now()`) and persisted deadlines used by safety/recovery decisions |
| Code evidence | Every persisted deadline is `Utc::now()` vs RFC3339 `TEXT`: approval TTL (`src/journal/approval.rs:36-59`), lease `locked_until` (`src/journal/worker.rs:43`, `src/journal/outbox.rs:58`), retry `available_at` (`src/journal/outbox_queue.rs:420-426`), proposal/approval `expires_at` (`src/server/capability_routes.rs:236`, `src/journal/capability_activation.rs:64-69`). Logical ordering is the Journal `sequence` (`migrations/0001_init.sql:34`, hashed in `src/journal/hash_chain.rs:3-13`). |
| Current owner | Kernel (reads host clock) |
| Durable state | Deadlines stored as `TEXT` columns on existing rows (not a separate Time object) |
| Security invariant | None of its own — deadlines are values compared in transition guards |
| Classification | **Environmental observation (L8)** + persisted deadlines already in existing state — NOT a primitive. See calculus §13. |
| Candidate derivation | `Time ≈ persisted deadlines (values in existing state) + logical sequence (K6) + host clock read (L8)` |
| External contracts | RFC3339 timestamp format convention |
| Migration risk | N/A (no Time object to migrate) |
| Current decision | **Document** — no Time primitive added; no Clock implemented. Four sub-domains distinguished (logical order / observation / duration / deadline). |
| Trigger to revisit | A verifiable/monotonic clock is required for lease safety under clock skew |

### 31. Grant Revocation (timing semantics)

| Field | Content |
|---|---|
| Current concept | The mechanism and timing of revoking an `external_operation_grants` row |
| Code evidence | Revocation is a `status` transition active→revoked (`src/journal/grant_ops.rs:192-224`); `revoked_at` is an audit stamp. Grants loaded **once at Run creation** (`src/runtime/hook_call.rs:236-255`) and frozen into `run.principal.grants`; effect dispatch reads the frozen copy via pure `evaluate_policy` (`src/gateway/policy.rs:66-101`); the live grant table is NOT re-read at dispatch. HCR revalidation (`src/hcr/revalidate.rs:34-130+`) does not touch the grant table. |
| Current owner | Kernel |
| Durable state | `external_operation_grants` (append-only; revoked rows retained) |
| Security invariant | Revocation is audit-retained (no row deletion); partial unique index on `status='active'` (`migrations/0006:44-49`) |
| Classification | **U** — disputed / under-evidenced semantic (see calculus §14) |
| Candidate derivation | Revocation is expressible with current candidates; the **timing** (A run-start pinning / B effect-time revalidation / C hybrid) is UNRESOLVED. |
| External contracts | grant_id, status strings, revoked_at |
| Migration risk | N/A (no code changed this round) |
| Current decision | **Revisit / hold** — behavior stays A (deferred); B and C are NOT implemented; no primitive is needed for any option. Requires a separately-reviewed ADR to decide A/B/C. |
| Trigger to revisit | An explicit revocation-latency requirement (e.g. "revoke within N seconds across in-flight Runs") is demanded |

---

## Screening-Focus Answers (questions answered, not implemented)

These answer the specific screening questions from the task. **None of them
authorize a code change.**

### Is Agent just Profile Binding + Scope + Snapshot + Run?
Yes, in the candidate model (`Agent ≈ K1 + K2 + K3 + K4`, row 1). The current
code already treats Agent as a config-loaded identity with no dedicated table.
**No migration is performed.**

### Is Session just Conversation Scope + ordered Events/Runs?
Yes, in the candidate model (`Session ≈ K2 + K6/K4`, row 3). The `sessions`
table plus the journal's ordered events realize exactly that. **No migration is
performed.**

### Can Multi-Agent be composed from External Proposer/Router + multiple Runs of different Profiles + correlation references?
Yes, in the candidate model. There is no multi-agent *code* to change: the
`agent_id` foreign key, `RunPrincipal`, and `correlation_id` on journal events
(`src/domain/mod.rs:376`) are already sufficient anchors. **This conclusion does
not add, delete, or modify any multi-agent code.**

### Should Router be an external Propose component (Kernel only validates)?
Conceptually yes (row 21). The Kernel already only creates the real Intent/Run
after the router matches. Externalizing the router is **not implemented** this
round; revisit when a third routing rule appears.

### Can Scheduler be external time logic → run.create Proposal?
Conceptually yes (row 22). It is **not implemented** — there is no scheduler
code. If scheduled runs are needed, build an external proposer; do not build a
Kernel cron platform.

### Is Hook ≈ Trigger × {Observe|Propose|Transform|Effect} × Contract × Component Binding?
As a conceptual lens, yes (row 17). The current code names hook kinds as
lifecycle points, not as these four verbs; the mapping is many-to-many. **The
Hook ABI is not renamed.**

### Is Approval a Human Subject's Decision over an Intent, with a stable facade?
Yes (row 10). Both the run-level and capability-change approval mechanisms are
Decision(K5) over an Intent(K5). **The Approval API is kept as the stable domain
facade; no Approval→Decision table migration occurs.**

### Is HCR a safe domain facade for Development Run (not to be deleted now)?
Yes (row 14). HCR is derivable as `Propose(dev Run) + 5×Receipt + Settlement
Decision`, but it is kept as the safe facade. **HCR is not generalized or
deleted this round.**

### Is Registry a Versioned Component Binding Snapshot (kept as-is)?
Yes (rows 11, 12). Registry ≈ Snapshot catalogue + Allow Boundary; Registry
Snapshot is K3. **Registry is not generalized this round.**
