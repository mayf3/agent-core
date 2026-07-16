# Agent Core: Kernel Primitive Calculus

> **Draft / Candidate Model — Not an immediate refactor plan.**
>
> This document proposes a candidate minimal set of irreducible Kernel
> primitives and a dual-track evolution strategy. **Nothing in this document is
> the current implementation fact.** The current implementation is described by
> the real source code under `src/`, the migrations under `migrations/`, and the
> per-concept evidence table in
> [`primitive-screening-matrix.md`](./primitive-screening-matrix.md). Treat this
> document as a long-lived thought model that future work *may* converge toward,
> not as a decision to migrate, rename, delete, or generalize any production
> object.

## 0. Status & Reading Order

| Section | Purpose |
|---|---|
| §1 | Why a dual track (inward screening + outward delivery) is required |
| §2 | The rule for judging whether something is a Kernel primitive |
| §3 | The candidate 8 (provisional) primitives; K8 disputed |
| §4 | The four external interaction modes (Observe / Propose / Transform / Effect) |
| §5 | Lean-like candidate syntax for invariants |
| §6 | Derivation formulas: how current domain objects decompose over the 8 |
| §7 | Primitive Gap Protocol |
| §8 | Generic Self-Evolution: current human gate vs. future external repair loop |
| §9 | OpenClaw as an external replacement goal (ADOPTED) vs. all-in-one Kernel (REJECTED) |
| §10 | Decision: screen only, do not slim — and the Non-Action List |
| §11 | Model Layers (sufficiency supplement: the candidate model is layered, not flat) |
| §12 | Liveness analysis (state machines, Attempt, Receipt terminal, temporal property) |
| §13 | Time model (four sub-domains, three expression options) |
| §14 | Snapshot vs. Grant semantic resolution (formula correction + revocation semantics) |
| §15 | North Star sufficiency table (which demand is expressible, gap, or unresolved) |

Cross-references to existing docs:

- Kernel boundary, tool/effect semantics, and storage direction:
  [`docs/architecture-rfc.md`](../architecture-rfc.md)
- External self-evolution rehearsal harness: [`docs/evolution-harness.md`](../evolution-harness.md)
- Phased product shape: [`docs/product-roadmap.md`](../product-roadmap.md)
- Dispatch / boundary rules for delegated coding agents:
  [`docs/agent-dispatch.md`](../agent-dispatch.md)
- Per-concept code evidence: [`./primitive-screening-matrix.md`](./primitive-screening-matrix.md)
- Extension hooks vs. external harness boundary:
  [`./extension-hook-and-external-harness-boundary-v0.md`](./extension-hook-and-external-harness-boundary-v0.md)

Where this document appears to conflict with the above, the existing RFCs and
the source code are authoritative. This document only records the conflict and
a suggestion; it does **not** modify their semantics.

---

## 1. Why a Dual Track

Agent Core has two simultaneous pressures, and they must not be collapsed into
one:

```text
Inward  : keep shrinking the Kernel to the irreducible primitives
Outward : keep shipping external capabilities without waiting for the Kernel to shrink
```

If only the inward track runs, external delivery stalls on a moving foundation.
If only the outward track runs, the Kernel accretes product logic and the
boundary erodes. The dual track keeps them decoupled:

### Inward track (concept screening, no external impact)

```text
continuous screening
  -> build compatibility facades over existing objects
  -> behavior-equivalence checks (replay/eval)
  -> small-step migration only when equivalence is proven
  -> never block external delivery
```

The inward track never deletes a domain object because it "looks derivable."
It deletes only after a facade has proven behavior equivalence across real
historical runs.

### Outward track (external delivery, validated against real demand)

```text
Generic DevelopmentRequest
  -> Contract Catalog
  -> Component Profile
  -> Deployment Lifecycle
  -> Repair Loop
```

These are then validated one at a time against concrete demand:

```text
Token Dashboard
Long-term Memory
Automatic Compression
Scheduled Briefing
Replaceable Router
Multi-profile Collaboration
Self-observation and Repair
```

When a real demand cannot be expressed by the candidate primitives, the Kernel
does **not** absorb the product logic. It triggers the Primitive Gap Protocol
(§7) and produces a `PrimitiveGapProposal` instead.

---

## 2. Primitive Qualification Rule

A concept qualifies as a **candidate irreducible Kernel primitive** only if it
satisfies **all** of:

1. **Non-derivable.** It cannot be defined as a pure composition of the other
   candidate primitives.
2. **Security load-bearing.** Removing it would either create a privilege
   boundary hole or make an existing invariant un-enforceable.
3. **Cross-cutting.** More than one external interaction mode depends on it.
4. **Stable shape.** Its contract has not churned across phases (or its churn
   is purely additive).

If a concept fails any criterion it is **not** a primitive — it is a derived
domain alias (D), an externalizable product capability (E), a phase scaffold
(S), or under-evidenced (U). See the matrix for the per-concept verdicts.

> This rule is a screening test, not a deletion mandate. A concept that *passes*
> is only a *candidate*; a concept that *fails* is **not** scheduled for removal.

---

## 3. Candidate 8 Primitives

The eight primitives below are the *candidate* set that, in this model,
everything else composes over. Each is cross-referenced to the real code that
*currently* plays that role (not to a future implementation).

> **Provisional-set note.** Seven of these (K1–K7) stand as candidate
> irreducible primitives. **K8 Allow Boundary is disputed / provisional** and is
> documented as such below (see the K8 note): it may not be an independent
> object primitive at all, but an inference rule / transition invariant enforced
> along Intent → Decision → Invocation. This document does **not** claim the
> eight primitives have been proven minimal and irreducible; that proof is the
> job of the screening matrix and future formal work.

| # | Candidate primitive | Role | Current code that carries it (evidence) |
|---|---|---|---|
| K1 | **Identity** | Stable, forgeable-proof identity of subjects, agents, runs, intents | `PrincipalId`/`AgentId`/`RunId`/`InvocationId`/`EventId` newtypes (`src/domain/mod.rs:37-41`); `RunPrincipal` (`src/domain/mod.rs:91-98`) |
| K2 | **Scope** | Namespaced conversation / execution scope used for isolation | `Session` keyed by `(agent_id, channel, conversation_key)` (`migrations/0001_init.sql:1-12`); `Session` struct (`src/domain/mod.rs:66-77`) |
| K3 | **Snapshot** | Immutable, content-addressed, pinned-at-creation state read | `RegistrySnapshot` (`src/registry/snapshot.rs:78`); runs pin `registry_snapshot_id` (`migrations/0002_registry_snapshots.sql:27`) |
| K4 | **Run** | A single auditable execution lifecycle for one scope | `Run` + `RunStatus` (`src/domain/mod.rs:146-192`); `runs` table (`migrations/0001_init.sql:14-25`) |
| K5 | **Intent + Decision** | A proposed side effect plus an authorization decision over it | `InvocationIntent` (`src/domain/mod.rs:258-265`), `ApprovedInvocation` (`src/domain/mod.rs:267-284`); the No-Effect-without-Decision gate in `src/domain/operation.rs:90` (`is_allowed`) |
| K6 | **Journal Event** | Append-only, hash-chained, monotonic fact log | `JournalEvent` (`src/domain/mod.rs:371-383`), `JournalEventKind` (`src/domain/mod.rs:404-536`); `journal_events` table + `previous_hash`/`hash` chain (`migrations/0001_init.sql:27-38`) |
| K7 | **Receipt** | Terminal outcome bound to exactly one invocation | `Receipt` + `ReceiptStatus` (`src/domain/mod.rs:286-300`); recorded as `outbox_dispatches` terminal transition + `ReceiptReceived` journal fact |
| K8 | **Allow Boundary** | The non-bypassable enforcement point that gates Intent→Effect | `Risk::Write` forcing the full intent→approval→adapter→receipt chain (`src/domain/operation.rs:90`); `RegistrySnapshot::provider_tools_for_grants` is the model-visible catalog surface |

> **Read carefully:** these are *candidate* primitives for a *future* model. The
> current code is **not** organized around these eight names, and the code must
> not be reorganized to match them in this round. They exist here only to make
> the screening in §6 precise.

### K8 is disputed / provisional

K8 (Allow Boundary) is **not** asserted to be a settled, independently
irreducible primitive. It is retained in the candidate table so the screening
in §6 has a label for the enforcement surface, but its status is open:

```text
Allow Boundary may not be a standalone object primitive at all.
It may instead be an unbypassable execution rule, or a safety
invariant, enforced on the Intent -> Decision -> Invocation transition.
```

Concretely, K5 already carries the authorization half (Decision) and the gating
check `is_allowed` (`src/domain/operation.rs:90`); much of what §3 lists under
K8 is the *enforcement of that K5 transition*, not a separable object. The
re-screening condition is recorded in
[`primitive-screening-matrix.md`](./primitive-screening-matrix.md):

```text
If every Allow Boundary semantic can be expressed as a transition
invariant over Intent + Decision + Invocation, then K8 is no longer
an independent primitive and demotes to an inference rule.
```

Because K8 is provisional, the candidate set is described as "8 (provisional)",
and **this document does not claim the eight primitives have been proven minimal
and irreducible.**

---

## 4. Four External Interaction Modes

Everything an external harness or connector does with the Kernel is one of four
modes. The Kernel's job is to enforce the contracts for each, not to implement
the external side.

```text
Observe   : read durable facts out of the Kernel (no side effect)
Propose   : submit a candidate Intent / DevelopmentRequest for Kernel decision
Transform : alter an in-flight payload (context block, intent) under contract
Effect    : perform a real side effect, gated by an Allow Decision + Receipt
```

| Mode | Direction | Kernel responsibility | Current carrier |
|---|---|---|---|
| Observe | external → read Kernel | expose durable facts; enforce read scope | `event.observe.v0` hook (`src/hook/types.rs:19-49`), SQLite read paths (`src/journal/sqlite_read.rs`) |
| Propose | external → submit to Kernel | schema-validate, authenticate, authorize, create the real Intent/Run | `/v1/ingress` gateway (`src/gateway/`); HCR claim + Run binding (`src/hcr/worker.rs:35`); capability proposal (`src/journal/capability_proposal_hcr.rs:11`) |
| Transform | external → mutate in-flight payload | re-validate non-bypassable invariants after the transform | `context.prepare.v0` hook inserting `ContextBlockKind::HookFragment` (`src/runtime/hook_call.rs:37-49`); RFC final guard (`docs/architecture-rfc.md` §5) |
| Effect | external → real world | mint Intent → get Allow Decision → dispatch adapter → record Receipt | outbox dispatch (`src/journal/outbox_queue.rs`), `InvocationAdapter` trait (`src/adapters/mod.rs:11`), `ReceiptReceived` fact |

The crucial invariant: **a Propose can never directly become an Effect.** Every
Effect must go through Intent → Decision → Receipt, regardless of who proposed
it. An external Proposer (router, scheduler, multi-agent orchestrator) can only
*submit*; the Kernel decides and dispatches.

---

## 5. Lean-like Candidate Syntax

This round keeps only pseudo-Lean / mathematical notation. **No Lean
dependency, no `lakefile`, no Lean source tree, no CI proof gate is added.** The
purpose of the notation is to make invariants precise enough to *later* map to
Rust property tests, TLA+/Alloy, or Lean — not to claim any are proven now.

### Candidate invariants worth formalizing

| Invariant | Best-fit technique (candidate) | Current enforcement |
|---|---|---|
| **No Effect without Allow Decision** | Rust property test (state-machine) | `is_allowed` + `ApprovedInvocation` gating (`src/domain/operation.rs:90`) |
| **Run Snapshot immutability** | Rust property test | content-addressed snapshot, run pins `registry_snapshot_id` (`migrations/0002_registry_snapshots.sql:27`); `RegistrySnapshot` is immutable |
| **Receipt binds exactly one Invocation** | Rust property test + DB constraint | `outbox_dispatches.invocation_id UNIQUE` (`src/journal/queue.rs:36-54`); HCR `hcr_receipt_identities UNIQUE(hcr_id, claim_id, run_id, idempotency_key)` (`migrations/0010_hcr_receipt_identity.sql:10`) |
| **External Proposal cannot mint Kernel Intent** | TLA+ / Alloy (cross-trust boundary) | Propose paths end at gateway/worker; the Kernel mints Run/Intent internally (`src/hcr/worker.rs:35`, `src/gateway/`) |
| **Journal append-only** | Rust property test + hash-chain verify | append-only `journal_events`, `verify_hash_chain` (`src/journal/sqlite.rs:300-318`) |
| **Scope isolation** | TLA+ / Alloy (multi-agent future) | per-session scoping today; cross-Agent isolation deferred (`docs/decisions/agent-home-directory-isolation.md`) |

### Pseudo-Lean sketch (illustrative, not compiled)

```lean
-- K5: the central safety theorem
theorem no_effect_without_allow
  (e : Effect) (h : occurred e) :
  ∃ (i : Intent) (d : Decision), allow d ∧ binds d i ∧ effect_of i = e :=
  sorry   -- not proven; a target for future property tests

-- K7: receipt uniqueness
theorem receipt_unique
  (r₁ r₂ : Receipt) (i : InvocationId)
  (h₁ : binds r₁ i) (h₂ : binds r₂ i) :
  r₁ = r₂ :=
  sorry

-- K6: journal monotonicity
theorem journal_append_only
  (j₁ j₂ : JournalEvent) (h : before j₁ j₂) :
  sequence j₁ < sequence j₂ ∧ prev_hash j₂ = some (hash j₁) :=
  sorry
```

`sorry` here is deliberate: it marks unproven targets. **The Kernel does not
claim these are formally proven.** The mapping in the table above says which
invariant is a good fit for which technique later.

---

## 6. Derived Concept Formulas

Each current first-class concept is expressed as a composition of the candidate
8 (provisional) primitives — K8 included as a label even though it is disputed
(§3). **These formulas describe a target decomposition, not current code.** Full
per-concept evidence and migration risk live in
[`primitive-screening-matrix.md`](./primitive-screening-matrix.md).

```text
Agent      ≈ Identity(K1) + Scope-bag(K2) + Snapshot-of-profile(K3) + Run(K4)
Principal  ≈ Identity(K1) + mutable grant set pinned-at-Run-start
              (grants are INDEPENDENT authorization state in
              `external_operation_grants`, minted-against but NOT derived from
              Snapshot(K3); see §14 for the correction and the deferred-
              revocation finding)
Session    ≈ Scope(K2) + ordered Journal Events(K6) / Runs(K4)
Run        ≈ K4 (primitive) pinned to Snapshot(K3) under Identity(K1) in Scope(K2)
Registry   ≈ Snapshot(K3) catalogue (capability definitions) + per-principal grant filter
Registry Snapshot = K3 (primitive)
HCR        ≈ a Propose(§4) for a development Run(K4) + 5 gate Receipts(K7) + Settlement Decision(K5)
Settlement ≈ terminal Decision(K5) reducing a set of Receipts(K7) + Journal Event(K6)
Capability Proposal ≈ Propose(§4) carrying artifact/manifest/evidence digests → Decision(K5) → Snapshot activation(K3)
Approval   ≈ a Human Subject's Decision(K5) over an Intent(K5), with a stable domain facade
Decision   ≈ the authorization half of K5 (primitive)
InvocationIntent ≈ the proposal half of K5 (primitive)
Invocation ≈ ApprovedInvocation = Intent(K5) + Decision(K5) → outbox dispatch
Receipt    ≈ K7 (primitive)
Hook       ≈ Trigger × {Observe|Propose|Transform|Effect}(§4) × Contract × Component Binding
Adapter    ≈ the Effect-side transport(§4) behind the Allow Boundary(K8)
Connector  ≈ external process speaking Observe(§4) + Effect(§4) over IPC
ContextBlock ≈ Transform-mode(§4) payload assembled per Run(K4)
Router     ≈ external Propose(§4) component; Kernel only validates + creates Intent(K5)
Scheduler  ≈ external time logic → run.create Propose(§4); NOT a Kernel cron platform
spawn      ≈ future cross-Scope(K2) Run(K4) creation; currently not_enabled
yield      ≈ future Run(K4) control-suspend; currently not_enabled
Capability ≈ Snapshot(K3) operation row + per-principal grant state (independent)
External Operation ≈ Snapshot(K3) row with BindingKind::External
                     + mutable per-principal grant state (external_operation_grants)
Workspace/Profile ≈ Identity(K1) + on-disk profile + mutable per-principal grant state (no first-class type today)
```

The recurring observation: a large share of "objects" are really one of the 8
primitives wearing a domain alias, or a composition that an external mode could
carry. The screening matrix records, per object, whether that composition is
safe to *expose* externally without touching internal storage.

---

## 7. Primitive Gap Protocol

When a real external demand cannot be met by composing the candidate 8
(provisional) primitives, the Kernel **must not** quietly absorb the product
logic. Instead:

```text
1. State the demand precisely (what the external world needs to do).
2. Show which composition of the 8 (K8 provisional) was attempted and why it failed.
3. Produce a PrimitiveGapProposal:
     - the missing primitive (or missing mode contract)
     - the security invariant it would carry
     - the minimal, additive change to expose it
     - the external delivery it unblocks
4. Review the proposal as a boundary decision (not a feature ticket).
5. Only after approval: add the primitive additively, behind a facade,
   with a behavior-equivalence check.
```

This protocol is the answer to "should the Kernel grow feature X?" If X is a
product capability, it belongs in an external harness. If X reveals a genuine
gap in the primitive set, it becomes a `PrimitiveGapProposal`. It is never the
case that X silently becomes a new Kernel object.

---

## 8. Generic Self-Evolution: Kernel Boundary vs. External Repair Loop

Self-evolution is already constrained by
[`docs/architecture-rfc.md`](../architecture-rfc.md) §8 and
[`docs/evolution-harness.md`](../evolution-harness.md). This section restates it
in primitive terms so the model is self-consistent, and — importantly —
separates the **Kernel's hard boundary** from the **External Evolution
Harness's** capabilities. The current "human gate" is a stage, not a permanent
axiom.

### What the Kernel never does (boundary, stable across all stages)

```text
The Kernel does not run the evolution loop.
The Kernel does not edit its own code.
The Kernel does not mint its own Decisions.
The Kernel does not bypass Approval.
```

These are invariant for every stage below. The distinction in later stages is
only about *who* produces the Decision (a human, or an external Policy Handler
proposing auto-approval) — never about the Kernel performing the evolution work
itself.

### What the External Evolution Harness may do (supported, regardless of stage)

```text
continuously Observe the Kernel's durable facts
diagnose problems
construct reproductions
develop patches
add regression tests
run the Gate
generate an Upgrade / Repair Proposal
verify deployment results
propose a rollback suggestion
```

The rehearsal harness is the external Proposer + Evaluator; the Kernel
contributes only the durable facts (K6), the pinned Snapshots (K3) for replay,
and records the Allow Decision (K5). An external harness may run the full
observe → diagnose → reproduce → patch → test → gate → propose loop. What it
cannot do is finalize an effective Decision inside the Kernel without going
through the Kernel's approval + journal path.

### Stage: current (today)

```text
Production merge / deploy still requires user approval.
Every effective Decision is minted and verified by the Kernel
only after an explicit human approval.
```

This is the present reality. It must not be read as a permanent theorem: it is
the current stage of a maturing external harness.

### Stage: future (allowed, not forbidden)

```text
Under all of:
  - no new privilege granted,
  - no change to a Contract,
  - a complete regression suite passes, and
  - a reliable rollback is available for the low-risk change,
an external Policy Handler may propose auto-approval.
The final effective Decision is still recorded and verified by the Kernel
through the same Intent -> Decision -> Invocation path.
```

The point of spelling this out: today's human gate is **not** promoted into a
permanent axiom that all upgrades must *forever* be a human PR merge. Low-risk
upgrades may, in the future, be auto-approved by an external Policy Handler
within the constraints above; the Kernel still records and verifies the
resulting Decision. Higher-risk changes remain human-gated.

### Loop sketch (labels the human vs. policy choice point)

```text
Observe failure (K6 / Observe)
  -> Attribute cause (external analyzer)
  -> Propose patch (external Propose / §4)
  -> Candidate branch or worktree (external)
  -> Static checks (external)
  -> Replay selected historical runs (Observe K6 + Snapshot K3)
  -> Evaluate into score/report (external evaluator)
  -> Human or external-Policy-Handler Decision (K5)
       [current stage: human only; future stage: policy may auto-approve
        low-risk changes under the constraints above]
  -> Promote by PR merge and tag (external)
       [current stage: manual merge/deploy under human approval]
  -> Rollback to last-known-good if needed (external)
```

Generic Self-Evolution = the External Evolution Harness runs the loop above; the
Kernel never runs the evolution loop, never auto-merges on its own, and never
edits its own `src/` during a run. The progression from "human gate today" to
"policy-approvable low-risk upgrades later" changes **who approves**, not the
Kernel's role.

---

## 9. OpenClaw as an External Replacement Goal (ADOPTED), Not an Internal Kernel Goal (REJECTED)

This section distinguishes two readings of "OpenClaw." One is the project's
current product direction; the other is an anti-pattern. They must not be
confused.

```text
REJECTED (the all-in-one internal reading):
  Suck Dashboard, Memory, Compression, Scheduler, Router, Multi-Agent,
  Workflow and similar features INTO the Kernel, producing an
  all-in-one OpenClaw-like Kernel. This enlarges the Kernel's security
  boundary and re-absorbs product logic the dual track pushes out.

ADOPTED (the external replacement reading):
  Keep a small Kernel. Through the feishu one-line flow, continuously
  develop, approve, deploy, and repair EXTERNAL Harness capabilities,
  progressively replacing the OpenClaw-style workloads the user
  actually runs today.
```

The adopted external North Stars include at least:

```text
Token Dashboard
Long-term Memory
Automatic Compression
Scheduled Briefing
Replaceable Router
Multi-profile Collaboration
Self-observation and Repair
```

These are the same capabilities the outward track in §1 validates one at a time
against concrete demand. They are **adopted product goals**, built and repaired
outside the Kernel through external Harness components — never by absorbing them
into the Kernel primitives.

Why the all-in-one internal reading remains REJECTED:

- It re-absorbs product logic the dual track is deliberately pushing out
  (workflow engine, multi-agent scheduler, long-term memory, dashboards — all
  listed as *external* in [`docs/product-roadmap.md`](../product-roadmap.md) and
  [`docs/architecture-rfc.md`](../architecture-rfc.md) §1).
- It makes the Kernel's security boundary larger, not smaller, contradicting
  the primitive qualification rule (§2).
- It conflicts with the established rule: *if a feature can be a plugin or an
  external loop, it should not be inside `core`* (`README.md` Key Principle).

The adopted North Star is therefore: a small, stable Kernel whose primitives
compose into everything else, with OpenClaw-style product capabilities growing
**externally** and progressively replacing the user's current OpenClaw
workloads.

---

## 10. Decision: Screen Only, Do Not Slim

**This round does not change any production object.** Screening produces a
verdict table and migration-risk notes; it does not execute migrations. The
reasons:

1. The candidate 8 (provisional — K8 is disputed, see §3) are a *model*, not yet
   validated by behavior-equivalence on real runs, and not proven minimal.
2. Several current objects carry durable external contracts (APIs, Journal event
   kinds, stable IDs, Receipt identity) that must not move until a facade proves
   equivalence.
3. External delivery (outward track) must not be blocked waiting for the Kernel
   to slim.

### Non-Action List (Not Now)

```text
- No existing table is deleted.
- No ID is migrated.
- No API is changed.
- No Journal event kind is changed.
- No North Star behavior is changed.
- No external harness development is blocked waiting for Kernel slimming.
- No domain object is deleted merely because it is conceptually derivable.
```

The full list of what is *forbidden in this round* (Agent→Profile migration,
Session→Scope migration, Approval→Decision table migration, HCR/Registry
generalization, Hook ABI rename, spawn/yield deletion, Scheduler implementation,
Router externalization, new Component Registry, new Workflow Engine) is binding
and is mirrored in the dispatch rules in [`docs/agent-dispatch.md`](../agent-dispatch.md).

---

## 11. Model Layers (Sufficiency Supplement)

> Added in the sufficiency-and-state-semantics round. **This section does not
> add or remove any primitive.** It states that the candidate model is *layered*,
> not a flat list of objects, and records a Lean-like *candidate* shape. It does
> **not** claim any property is proven.

The candidate 8 (provisional) primitives in §3 are **state carriers**. But a
complete model of the Kernel must distinguish *layers* that the §3 list alone
conflates. The table below separates them. Each row is a *category of model
element*; the candidate primitives from §3 populate the rows marked "state
carriers" only.

| # | Layer | What it is | Current carrier (evidence) |
|---|---|---|---|
| L1 | **State** | The durable values the Kernel remembers | `runs`, `sessions`, `journal_events`, `outbox_dispatches`, `worker_jobs`, `external_operation_grants`, `registry_snapshots` (tables under `migrations/`); newtypes in `src/domain/mod.rs:37-41` |
| L2 | **Input / Command** | What an external actor submits to mutate state | `/v1/ingress` Propose (`src/gateway/`); HCR claim + Run binding (`src/hcr/worker.rs:35`); `/v1/approve`/`/v1/deny` (`src/gateway/mod.rs:368-465`); capability proposal (`src/journal/capability_proposal_hcr.rs:11`) |
| L3 | **Transition relation** | The rules by which `(State, Input) → State'` is permitted; the guard that decides whether a step is legal | `evaluate_policy` (`src/gateway/policy.rs:66-101`); outbox terminal-transition guard `TERMINAL_TRANSITION_ERROR` (`src/journal/outbox_queue.rs:8`); CAS guards in `activate_registry_tx` (`src/journal/activation_core.rs:73-126`) |
| L4 | **Effect** | The real-world side effects a transition may emit | outbox dispatch (`src/journal/outbox.rs`); `InvocationAdapter` trait (`src/adapters/mod.rs:11`); `ReceiptReceived` fact |
| L5 | **Trace** | The append-only, hash-chained sequence of facts produced by transitions | `journal_events` table + `previous_hash`/`hash`/`sequence` (`migrations/0001_init.sql:27-38`); `verify_hash_chain` (`src/journal/sqlite.rs:300-318`) |
| L6 | **Safety invariant** | A property that must hold on **every** reachable state | No-Effect-without-Decision (`is_allowed`/`evaluate_policy`); Receipt binds exactly one invocation (`outbox_dispatches.invocation_id UNIQUE`); Journal append-only + hash chain |
| L7 | **Liveness property** | A property that asserts something *eventually* happens (a temporal property, not a state shape) | delivery + retry + dead-letter (§12); recovery of `Unknown` runs (`src/journal/unknown.rs`) |
| L8 | **Environmental observation** | Inputs the Kernel reads from its environment that are **not** submitted commands — clock reads, host facts | `Utc::now()` reads (§13); `examples/time_harness.rs` answering `external.time_now` |

**Why the layering matters for sufficiency.** Several questions in the task —
"is Liveness a primitive?", "is Time a primitive?" — only become answerable
once L7 (liveness) and L8 (environmental observation) are recognized as
*distinct layers*. A liveness property is **not** a state carrier, so it cannot
be a §3-style primitive by definition; it is a temporal property that must be
*enforced by* the transition relation (L3). The §12 and §13 analyses use this
layering. **This section makes no claim that the layering is complete or proven;
it records a candidate decomposition.**

### Lean-like candidate step shape (illustrative, not compiled)

The transition relation (L3) has a candidate shape. This is a *target sketch*
for future property tests — **it is not proven, and nothing depends on it
compiling.**

```lean
-- Candidate shape of one Kernel step (L3 transition relation).
-- NOT proven; illustrative only.
step : State → Input → State × List Effect
```

Read as: given the current `State` and an `Input`, the transition relation
returns a new `State` and a (possibly empty) list of `Effect`s. Safety
invariants (L6) are predicates over `State` that must hold before *and* after
every `step`. Liveness properties (L7) are temporal predicates over the *trace*
(L5) of states produced by a sequence of `step`s — e.g. "every `Dispatching`
outbox row eventually reaches a terminal state (Succeeded/Failed/Unknown/Dead)."

> **Not claimed.** The Kernel does not assert this shape is implemented, that
> `step` totalizes over all `(State, Input)` pairs, or that any liveness
> property is formally proven. The sketch exists only to make §12/§13 precise.

---

## 12. Liveness Analysis

> Added in the sufficiency round. **This section does not add a Liveness
> primitive.** It records the existing liveness machinery in the code, answers
> four specific questions, and concludes whether liveness is a primitive, a
> transition relation, or a temporal property.

The candidate primitives in §3 are all *state carriers* (L1). Liveness is
different: it is the question of whether the system **eventually makes
progress** — e.g. does a queued dispatch terminate, does a stale lease get
reclaimed, does an `Unknown` run get reconciled. This is a **temporal property
(L7)**, not a state shape. This section distinguishes the two.

### Existing liveness machinery (real code)

The Kernel already implements reliable-effect delivery machinery, but it was
not represented in §3. Concretely:

| Mechanism | State machine | Evidence |
|---|---|---|
| **Outbox dispatch** | `OutboxDispatchStatus`: `Pending → Dispatching → {Succeeded, Failed, RetryableFailed → (backoff) → Dispatching, Unknown, Dead}` | `src/domain/status.rs:42-82`; transitions in `src/journal/outbox_queue.rs` + `src/journal/outbox.rs:10-105` |
| **Worker job** | `WorkerJobStatus`: `Queued → Running → {Succeeded, Failed, RetryableFailed → (backoff) → Running, Dead}` | `src/domain/status.rs:3-40`; lease loop `src/journal/worker.rs:10-88` |
| **Lease + reclaim** | 5-minute lease (`Duration::minutes(5)`); stale-lease reclaim predicate `locked_until <= now` | `src/journal/worker.rs:43,52-56`; `src/journal/outbox.rs:58,62-66` |
| **Retry + backoff** | exponential `base * 2^(attempts-1)` capped at `max_retry_delay_ms`; `available_at` persisted deadline | `src/domain/retry.rs:22-31`; applied `src/journal/worker.rs:235-241`, `src/journal/outbox_queue.rs:420-426` |
| **Dead-letter** | terminal `Dead` after `max_*_attempts`; never re-leased | `src/journal/worker.rs:272-303`; `src/journal/outbox_queue.rs:458-498` |
| **Unknown recovery** | `DispatchStarted` with no terminal fact → `Unknown` + run status set to `Unknown` | `src/journal/unknown.rs:75-146` |

These two state machines are the **reliable-effect delivery substrate**. They
are distinct from any external Scheduler/Cron (see the distinction below).

### Q1. Can the worker/outbox intermediate states be modeled as an Invocation/Attempt state machine?

**Partially, and with a caveat.** The outbox dispatch row *is* effectively an
attempt state machine for one `invocation_id`: it carries `status`, `attempts`
(an integer counter, `queue.rs:47`), `available_at`, `locked_by`,
`locked_until`, and `last_error`. The transitions in
`src/journal/outbox_queue.rs` realize a recognizable attempt lifecycle
(`Pending → Dispatching → terminal-or-retryable → Dead`).

The caveat: there is **no general first-class `Attempt` type** — an attempt is
the integer `attempts` counter on the row, not a separately-addressable object.
The HCR flow *does* have a first-class attempt type (`HcrGateAttempt`,
`src/domain/harness_change_request.rs:145`, table `hcr_gate_attempts` in
`migrations/0009_hcr_evidence.sql:5-19` with a 1:1 binding to an invocation
intent and a 1:1 binding to a receipt event), but that is HCR-specific, not a
general Kernel object.

### Q2. Does K7 Receipt carry only terminal evidence?

**Yes — Receipt is terminal evidence bound to exactly one invocation.**
`ReceiptStatus` has exactly three variants: `Succeeded`, `Failed`, `Unknown`
(`src/domain/mod.rs:295-300`). `ReceiptReceived` is written only on terminal
transitions of the outbox (`succeed_outbox_dispatch`, `fail_outbox_dispatch`;
`src/journal/outbox_queue.rs:148-159,315-326`), and only safe (sanitized) fields
are journaled. The `Unknown` terminal does *not* write `ReceiptReceived` — it
writes `OutboxDispatchUnknown` instead (`src/journal/unknown.rs:85-97`), a
deliberate choice. So K7 Receipt is exactly "terminal-outcome evidence for one
invocation"; it carries no retry/intermediate semantics.

### Q3. Is "Attempt" a derived object, an internal implementation structure, or a candidate new primitive?

**A derived object / internal implementation structure — not a candidate
primitive in this round.** An attempt is the `attempts` counter + the
non-terminal lifecycle of an outbox/worker row. It is fully derivable from the
dispatch row's state; it has no stable cross-cutting contract of its own (the
HCR `HcrGateAttempt` is a domain-specific generalization, not a Kernel-wide
contract). Promoting Attempt to a primitive would require showing it carries a
security invariant the existing state machine does not — which the evidence does
not support. **No Attempt primitive is added.**

### Q4. Is liveness a primitive, a transition relation, or a temporal property?

**A temporal property (L7), enforced *by* the transition relation (L3) — not a
state-carrying primitive.** Liveness assertions like "every `Dispatching`
outbox row eventually reaches a terminal state" are predicates over the trace
(L5), not values stored in state. They are realized by the retry/backoff +
dead-letter + unknown-recovery machinery above. They cannot be a §3-style
primitive (which are all state carriers). **No Liveness primitive is added.**
See §13 for the related Time question.

### Kernel reliable-effect delivery ≠ external Scheduler / Cron

These must not be conflated:

```text
Kernel reliable-effect delivery (this section)
  = the in-Kernel guarantee that an approved invocation's effect is
    delivered at-least-once, retried with backoff, dead-lettered, and
    reconciled if its outcome is Unknown. This is a SAFETY + liveness
    substrate for the Intent -> Decision -> Effect chain (K5).

External Scheduler / Cron (row 22 of the matrix)
  = an external Proposer(§4) that decides WHEN to create a new Run,
    based on wall-clock time or recurrence. The Kernel does not host
    this. (See §13 for why Time alone does not justify a Scheduler primitive.)
```

**The existence of retry/backoff machinery does NOT justify adding a Scheduler
Kernel primitive.** Retry is part of reliable delivery; scheduling is an
external Propose concern. These are different layers.

---

## 13. Time Model

> Added in the sufficiency round. **This section does not add a Time primitive,
> does not implement a Clock, and does not change any production code.** It
> classifies every time-based safety/recovery read, evaluates three expression
> options, and records a judgment.

### Four distinct time sub-domains actually present

A code audit of every `Utc::now()` and persisted deadline shows the Kernel uses
time in **four categorically different ways**. Conflating them is the root cause
of the "is Time a primitive?" confusion.

| Sub-domain | Definition | Current carriers (evidence) |
|---|---|---|
| **(a) Logical order / Journal sequence** | Monotonic integer counter; **NOT** wall-clock. Authority for event ordering and mixed into the hash chain. | `journal_events.sequence` (`migrations/0001_init.sql:34`); `append_event_tx` reads `MAX(sequence)+1` (`src/journal/queue.rs:127-183`); `sequence` is hashed (`src/journal/hash_chain.rs:3-13`); `registry_state.version` CAS counter (`src/journal/activation_core.rs:73-126`) |
| **(b) Wall-clock observation** | `Utc::now()` read purely to stamp a record (audit/log); **no decision depends on it.** | `created_at`/`updated_at`/`decided_at` stamps across `src/journal/sqlite.rs`, `src/registry/`, `src/domain/`; `revoked_at` is an audit stamp, not the revocation decision (the `status` transition is — `src/journal/grant_ops.rs:200-205`) |
| **(c) Monotonic duration** | A relative delta (e.g. "lease for 5 min", "backoff N ms"); used to *compute* a deadline. | 5-min lease `Duration::minutes(5)` (`worker.rs:43`, `outbox.rs:58`); backoff `next_retry_delay_ms` (`src/domain/retry.rs:22-31`); approval TTL `write_approval_ttl_secs` (`src/config.rs:55`) |
| **(d) Persisted deadline** | A stored absolute timestamp a safety/recovery decision compares against `Utc::now()`. **The load-bearing category.** | See the table below |

### Persisted deadlines that gate safety/recovery decisions

| Decision | Persisted deadline | Evidence |
|---|---|---|
| Fail stale `AwaitingApproval` run | journal event `created_at` vs `now - ttl` | `src/journal/approval.rs:36-59` |
| Worker/outbox lease reclaimable | `locked_until` vs now | `src/journal/worker.rs:22-28`; `src/journal/outbox.rs:22-24` |
| Worker/outbox retry re-admissible | `available_at` vs now | `worker.rs:24-25`; `outbox.rs:22-23` |
| Stale-lease health | `locked_until <= now` | `src/journal/queue_health.rs:142-155,74-87` |
| Unknown-invocation recovery | `locked_until <= now` (+ fact presence) | `src/journal/unknown.rs:155-202` |
| Capability proposal expiry | `capability_change_proposals.expires_at` vs now | `src/server/capability_routes.rs:236`; in-tx re-check `src/journal/capability_activation.rs:64-69` |
| Trusted-approval expiry | `capability_change_approvals.expires_at` vs now | `src/server/capability_decision.rs:52-59`; `src/journal/trusted_capability_activation.rs:338-341`; sweep `src/journal/activation_core.rs:291-294` |

**Observation (not an action):** every persisted deadline is `Utc::now()`
compared against RFC3339 `TEXT` columns. There is **no monotonic-clock
abstraction**; two inconsistent comparison conventions coexist (`< now` vs
`>= expiry`). Also, `RetryPolicy.lease_timeout_ms` (`src/domain/retry.rs:7,18`,
default 30000) is **vestigial** — the actual lease is hardcoded
`Duration::minutes(5)`, so configuring it has no effect. These are recorded as
observations; **no code is changed this round.**

### Three expression options (judgment recorded, not implemented)

The task asks to evaluate three ways Time *could* be modeled. This round only
records the judgment for each; none is implemented.

| Option | Description | Judgment this round |
|---|---|---|
| **(1) Time as an independent primitive** | Add a `Time`/`Clock` primitive to §3 | **NOT PROVEN.** The four sub-domains above show Time is not a *state carrier*; the load-bearing uses (d) are persisted deadlines, which are already values stored in existing state (outbox/worker rows, proposal/approval tables). Adding a Time primitive would duplicate state that already exists. The logical-order sub-domain (a) is already the Journal `sequence` (K6). No independent Time primitive is justified by the evidence. |
| **(2) TimeObservation as an Input (L2)** | Model clock reads as environmental inputs to the transition relation | **Plausible as a modeling lens**, and consistent with L8 (environmental observation). It captures the truth that `Utc::now()` is read from the host, not stored as Kernel state. But it does not require a new primitive — it is a way of *describing* how existing transitions read sub-domain (d) deadlines. |
| **(3) Clock as a trusted environment boundary** | Treat the host clock as a trust boundary the Kernel reads but does not own | **Consistent with the existing architecture** (the Kernel already trusts the host for `Utc::now()` and for `examples/time_harness.rs`). This is a *boundary statement*, not a primitive. It would matter if the Kernel ever needed a verifiable/monotonic clock (e.g. for lease safety under clock skew) — a future concern, **not acted on this round.** |

**Conclusion:** none of the three options is PROVEN to require a new primitive.
The load-bearing time semantics are **persisted deadlines (d)** already stored
in existing state, plus **logical sequence (a)** already covered by K6. **No
Time primitive is added; no Clock is implemented.**

---

## 14. Snapshot vs. Grant Semantic Resolution

> Added in the sufficiency round. **This section does not change any production
> code.** It corrects a wrong formula in §6, records the grant/snapshot
> relationship precisely, and reports the deferred-revocation finding.

### The correction

The original §6 formula

```text
Principal ≈ Identity(K1) + grant set derived from Snapshot(K3,K8)
```

is **wrong**. Grants are **not** derived from the snapshot. Corrected formulas
were applied in §6 above and are restated here for clarity:

```text
RegistrySnapshot  = K3 (primitive)
                    an immutable, content-addressed capability/binding CATALOGUE.
                    Contains OperationSpec rows (name, risk, parameters,
                    binding_kind). Carries NO principal and NO authorization
                    state. (src/registry/snapshot.rs:77-82,98-107)

ExternalOperationGrant = independent, mutable, revocable AUTHORIZATION STATE
                    (status: active|revoked; revoked_at audit stamp) persisted
                    in external_operation_grants, minted-AGAINST a snapshot_id
                    but NOT computed from snapshot contents.
                    (src/domain/operation.rs:310-321;
                     migrations/0006_external_operation_grants.sql:1-6,22-49)
```

The migration header is explicit about the separation:
*"Separates grant authorization from capability activation: activating a
capability only registers the operation in the registry snapshot; a grant is
required for a specific principal to invoke it."*
(`migrations/0006_external_operation_grants.sql:1-6`)

### The grant ↔ snapshot relationship (intersection, not derivation)

The model-visible tool catalog is computed by
`RegistrySnapshot::provider_tools_for_grants` (`src/registry/snapshot.rs:101-107`).
It takes the grant list as a **caller-supplied parameter** and **intersects** it
with the snapshot's operation definitions:

```text
provider_tools_for_grants(snapshot, granted)
  = { op in snapshot.operations
        | op.name in granted
        | op.is_visible_to_provider() }
```

It does **not** read the grant table. The snapshot contributes operation
*definitions*; the grants contribute the *authorization filter*. Calling grants
"derived from the snapshot" inverts the relationship. The same intersection
shape is used at both call sites (`src/runtime/mod.rs:205-234`,
`src/runtime/tool_loop.rs:43-51`).

### When grants are loaded, and whether dispatch revalidates them

| Question | Answer | Evidence |
|---|---|---|
| Does a Run pin a snapshot? | **Yes** — `runs.registry_snapshot_id` (`migrations/0002_registry_snapshots.sql:27`), written at `src/runtime/hook_call.rs:268` | |
| When are grants loaded? | **Once, at Run creation** — `create_run` merges (a) owner-coding grants derived from snapshot ops (`src/runtime/coding_grants.rs:38-63`) and (b) active rows from `external_operation_grants` filtered by the pinned `snapshot_id` (`src/runtime/hook_call.rs:236-255`), then freezes the result into `run.principal.grants` and persists the Run. | |
| Does effect dispatch re-check the live grant table? | **No.** `evaluate_policy` is pure ("no I/O, no Gateway state, no mutation", `src/gateway/policy.rs:49-65`) and reads only the borrowed `run.principal.grants` (the frozen copy) + the pinned snapshot (`src/gateway/policy.rs:66-101`). The sole non-test read of `load_active_external_operation_grants` is the run-start site. | |
| Does HCR-mode dispatch re-check grants? | **No.** HCR per-dispatch revalidation (`src/hcr/revalidate.rs:34-130+`) rechecks RunMode/HCR/claim/owner/channel identity but **never queries `external_operation_grants`**. | |

### Revocation semantics

Because grants are pinned at Run start and never re-read during the Run,
revoking a grant takes effect **on the next Run** for that principal — **not**
on in-flight tool calls within the current Run. This is **deferred (run-start
pinning)**, not immediate.

**Is this a real safety contradiction?** The docs do **not** claim immediate
revocation. §6's original (now-corrected) formula "grant set derived from
Snapshot" did *imply* a tighter coupling than exists, but no document asserted
"revocation takes effect immediately within an in-flight Run." Therefore this is
recorded as a **disputed / under-evidenced semantic (Classification = U)**, not
a High-severity contradiction. It is a decision the project has not yet made
explicitly:

```text
A. Run-start grant pinning        (current behavior)
   Grants are resolved once at create_run and frozen for the Run.
   Revocation affects the NEXT Run only.

B. Effect-time grant revalidation (not implemented)
   Each dispatch re-reads the live grant table. Revocation is immediate,
   even within an in-flight Run.

C. Hybrid: Registry pinned, high-risk Effect revalidated (not implemented)
   The capability catalogue (snapshot) is pinned for determinism, but a
   high-risk Effect re-reads the live grant table before dispatch.
```

**Classification = U / disputed.** No ADR or test in the repository decides
among A/B/C. This round does **not** change the behavior (it stays A) and does
**not** implement B or C. The finding is recorded for a future, separately-
reviewed decision.

---

## 15. North Star Sufficiency Table

> Added in the sufficiency round. This table evaluates each North Star against
> the candidate primitives and assigns one of four classifications. **"Not yet
> implemented" is NOT automatically `GENUINE_PRIMITIVE_GAP`.** A gap is only
> "genuine" when the demand *cannot* be expressed by composing the candidate
> primitives — i.e. it would require a new state-carrying primitive (§7
> Primitive Gap Protocol).

### Classification legend

```text
EXPRESSIBLE_WITH_CURRENT_CANDIDATES
  The North Star can be composed from the candidate 8 (provisional) primitives
  + the four interaction modes (§4). What is missing is external harness
  implementation, not a Kernel primitive.

CONTRACT_OR_IMPLEMENTATION_MISSING
  The composition is clear, but a specific API, profile, contract, or
  deployment harness does not yet exist. Adding it does NOT add a primitive.

SEMANTICS_UNRESOLVED
  The composition is plausible but depends on a semantic decision the project
  has not made (see §14 A/B/C). Cannot be classified until that decision lands.

GENUINE_PRIMITIVE_GAP
  The demand CANNOT be met by composing the candidate primitives. Requires a
  PrimitiveGapProposal (§7). Reserved for the strongest case only.
```

### Sufficiency table

| North Star | Classification | Composition formula & failure point |
|---|---|---|
| **Token Dashboard** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | An external Observe(§4) component reading durable facts (K6 Journal events, K4 Run status) and rendering usage. The Kernel already exposes read paths (`src/journal/sqlite_read.rs`). Missing: an external dashboard component — not a primitive. |
| **Long-term Memory** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | External memory store keyed by Identity(K1)/Scope(K2); the Kernel contributes Session(K2) + ordered Events(K6). Compression summarization pointer already exists on sessions. Missing: external memory component — not a primitive. |
| **Automatic Compression** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | A `context.compress.v0` hook (Transform mode, §4) already exists (`src/hook/types.rs:19-49`); ContextBlock has `Compressibility` (`src/domain/context_block.rs:36-42`). The composition is `Transform(§4) payload per Run(K4)`. Missing: a compression policy/component — not a primitive. |
| **Scheduled Briefing** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | An external Scheduler (row 22) Propose(§4) creating a Run(K4) on a schedule. The Kernel needs no cron platform (§12). Missing: external scheduler + briefing profile — not a primitive. (§13 shows Time alone does not justify a primitive.) |
| **Replaceable Router** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | Router = external Propose(§4); the Kernel only validates + creates Intent(K5)/Run(K4) (row 21). Current routers are in-process by design. Externalizing is an implementation step — not a primitive. |
| **Multi-profile Collaboration** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | Multiple Runs of different profiles composed via Identity(K1) + Scope(K2) + `correlation_id` on journal events (`src/domain/mod.rs:376`). `agent_id` foreign key already supports it (row 1). Missing: multi-profile runtime/profile type (row 27) — implementation, not a primitive. |
| **Self-observation and Repair** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | The external Evolution Harness loop (§8): Observe(K6) → diagnose → Propose patch → Gate → Decision(K5) → deploy. The Kernel contributes durable facts + pinned Snapshots(K3) + records the Decision. Missing: harness maturity — not a primitive. |
| **Rollback** | `EXPRESSIBLE_WITH_CURRENT_CANDIDATES` | See the Rollback derivation below. A rollback is a sequence of existing primitives, not a new one. |
| **Grant Revocation** | `SEMANTICS_UNRESOLVED` | Grant revocation EXISTS (`src/journal/grant_ops.rs:192-224`) and is expressible (grant = independent state). But the *timing semantics* (immediate vs deferred) is the §14 A/B/C dispute. Classified `SEMANTICS_UNRESOLVED`, not a primitive gap — no new primitive is needed for any of A/B/C. |

### Rollback derivation

The task asks to verify that `rollback` decomposes over existing primitives
rather than requiring a new `Rollback` primitive. The candidate derivation:

```text
rollback
  = Intent(activate a historical snapshot/artifact)   -- K5 Intent
  -> Decision (human or policy approval)              -- K5 Decision
  -> Deployment Effect (activate registry snapshot)   -- §4 Effect
  -> Receipt                                          -- K7 Receipt
  -> active binding changed Event                     -- K6 Journal Event
```

Does the current activation mechanism support this derivation? **Yes, for the
registry-snapshot case.** Snapshot activation is already atomic and journaled:
`activate_registry_tx` performs a CAS on `registry_state` + appends a journal
event inside `BEGIN IMMEDIATE` (`src/journal/activation_core.rs:73-126`), and
`activate_snapshot_transactional` (`src/journal/registry_ops.rs:203`) wraps CAS
+ journal. Capability activation (`activate_proposal_atomic`,
`src/journal/capability_activation.rs:51-70`) re-reads `expires_at` in-transaction
as a TOCTOU guard. So "activate a historical snapshot" is already an Effect that
produces a changed-binding Event + Receipt.

What is missing is **not a primitive** — it is the external harness/API surface
to *request* a rollback (a Profile/Deployment Harness that issues the Intent and
records which historical artifact to activate). That is a
`CONTRACT_OR_IMPLEMENTATION_MISSING` concern, not a `GENUINE_PRIMITIVE_GAP`.
**No Rollback primitive is added.**

### Tally of classifications

```text
EXPRESSIBLE_WITH_CURRENT_CANDIDATES : 8  (Token Dashboard, Long-term Memory,
     Automatic Compression, Scheduled Briefing, Replaceable Router,
     Multi-profile Collaboration, Self-observation and Repair, Rollback)
CONTRACT_OR_IMPLEMENTATION_MISSING  : 0  (folded into the EXPRESSIBLE rows
     as "missing: external component", since none requires a new primitive)
SEMANTICS_UNRESOLVED                : 1  (Grant Revocation — §14 A/B/C)
GENUINE_PRIMITIVE_GAP               : 0
```

**No North Star requires a new primitive this round.** Two require follow-up:
Grant Revocation needs the A/B/C semantic decision; the EXPRESSIBLE rows need
external harness implementation (outward track, §1), which is explicitly not
blocked by Kernel screening (§10).

---

## Appendix A: Conflicts with Existing Docs

Recorded conflicts (this document records them; it does not resolve them):

1. **RunPrincipal channel enum.** [`docs/architecture-rfc.md`](../architecture-rfc.md)
   §4 lists `channel: "cli" | "feishu" | "api" | "cron"`. The *current code*
   (`src/domain/mod.rs:79-83`) only implements `Cli` and `Feishu`; `cron` is a
   future channel per the roadmap, not present code. This model treats Scheduler
   as an external Proposer (§6) rather than a `cron` channel, which would make
   the RFC's `cron` channel literal redundant. **Suggestion only:** reconcile in
   a future RFC update; do not edit the RFC now.
2. **Hook verbs.** This model speaks of Observe/Propose/Transform/Effect as hook
   subtypes. The *current code* names hook kinds as lifecycle points
   (`ingress.route.v0`, `context.prepare.v0`, `context.load.v0`,
   `context.compress.v0`, `event.observe.v0`, `decision.policy.v0`,
   `src/hook/types.rs:19-49`). The mapping between the two vocabularies is many-
   to-many and is not enforced. **Suggestion only:** keep the lifecycle-point
   naming as the code authority; treat the four verbs as a conceptual lens.
3. **ExternalSystemManifest.** [`docs/architecture-rfc.md`](../architecture-rfc.md)
   §6 sketches `ExternalSystemManifest`/`ExternalSystemAdapter` with a
   `trustLevel`. The current code implements registry snapshots + harness
   manifests (`migrations/0002`, `0003`) rather than that exact interface. This
   model's Snapshot(K3) + Allow Boundary(K8) is closer to the implemented shape.
   **Suggestion only:** no change now.

None of the above authorizes editing the RFCs. They are recorded so a future,
separately-reviewed RFC update can address them.

---

## Appendix B: Branch Integration Note

This `docs/kernel-primitive-calculus-v0` branch was cut from `main` at a point
where the root-directory audit report was still present.

```text
This branch should be integrated against the latest origin/main AFTER
both land:
  1. the event.observe High fix, and
  2. the root-report guard (which removes / forbids the
     root-directory audit report).

Do not delete the root-directory audit report on this branch directly;
let it be resolved by the root-report guard and a rebase onto origin/main.
```

This document is documentation-only; the integration note does not authorize
any production change on this branch.

---

## Appendix C: Sufficiency & State-Semantics Review Verdicts

> Added in the sufficiency-and-state-semantics round. These are the authoritative
> verdicts for the round. All are documentation-only; **no production behavior
> changed**.

```text
Liveness omission:                      CONFIRMED
  (reliable-effect delivery machinery existed in code but was not modeled;
   now documented in §12 as a temporal property L7, not a primitive)

Time modeling omission:                 CONFIRMED
  (every safety/recovery time read was Utc::now() vs RFC3339 text with no
   monotonic-clock abstraction; four sub-domains now distinguished in §13)

Snapshot/grant formula issue:           CONFIRMED
  (the §6 formula "grant set derived from Snapshot(K3,K8)" was wrong;
   grants are independent mutable authorization state; corrected in §6 + §14)

Immediate revocation semantics:         UNRESOLVED
  (revocation is deferred — run-start pinning — but no doc asserted immediate
   revocation, so this is a disputed semantic §14 A/B/C, Classification = U,
   not a High-severity contradiction; no code changed)

New Time primitive:                     NOT_PROVEN
  (load-bearing time semantics are persisted deadlines already in existing
   state + logical sequence already in K6; §13 records the judgment)

New Liveness primitive:                 NOT_PROVEN
  (liveness is a temporal property L7 enforced by the transition relation,
   not a state carrier; §12 records the judgment)

K1/K2 reducibility:                     NOT_PROVEN
  (Identity K1 and Scope K2 carry distinct security responsibilities —
   forgeable-proof identity vs. isolation namespace — and both are
   cross-cutting; neither is derivable from the other; see the matrix)

K4/K5/K7 reducibility:                  NOT_PROVEN
  (Run K4, Intent+Decision K5, and Receipt K7 occupy distinct lifecycle
   roles — execution lifecycle vs. authorization gate vs. terminal evidence —
   with distinct stable contracts; see the matrix)

Primitive count changed:                NO
  (the candidate set remains 8 provisional; no primitive added, merged,
   split, or removed)

Production behavior changed:             NO
  (docs-only; no Rust/TS, migration, API, table, or runtime change)

External Harness mainline blocked:      NO
  (screening does not block the outward track; §10 Non-Action List holds)
```

### Round status

```text
KERNEL_PRIMITIVE_SUFFICIENCY_AND_STATE_SEMANTICS_DOCUMENTED_READY_FOR_REVIEW
```

---

# 16. 关键边界：内核不理解"怎么做"

本节是本文件的强约束，不是建议。

## 16.1 内核只负责治理事实，不负责工作方法

内核可以知道：

```text
谁提出请求
请求属于哪次运行和隔离范围
请求调用哪个受信任的外部执行器
请求哪些权限
是否允许
实际执行了什么
执行结果和证据是什么
当前启用哪个版本
```

内核不得理解：

```text
为什么需要这个功能
如何拆任务
先让哪个角色执行
采用什么研发方法
如何设计页面
如何生成代码
如何修复编译错误
如何判断业务需求是否完成
多 Agent 如何分工和收敛
```

一句话概括：

```text
Kernel 负责：
谁、哪次运行、是否允许、执行了什么、结果是什么、启用了哪个版本。

External Harness 负责：
想做什么、怎么做、让谁做、怎么验收、失败后怎么办。
```

## 16.2 开发计划不是内核原语

开发计划、任务拆解、角色安排和执行顺序全部属于外部规划与编排。

正确链路：

```text
用户请求
→ 外部路由/规划 Harness
→ 外部生成开发方案
→ 外部多 Run / 多角色编排
→ Coding Harness
→ 外部验收 Harness
→ 候选产物
→ 内核治理批准、部署授权、结果记录和版本切换
```

内核不需要保存计划内容，也不需要理解计划结构。

在确有审计需要时，内核最多可以记录一个不透明引用：

```text
plan_ref
plan_digest
```

其含义仅是：

```text
"本次批准或执行引用了这一份外部制品"
```

内核不得解析计划步骤、角色、依赖图或业务目标。

如果最终只需绑定：

```text
原始请求
候选产物
验收证据
批准结果
部署回执
```

则连 `plan_ref` 都不必记录。

## 16.3 验收规则不属于内核

业务验收属于外部验收 Harness。

例如 Token Dashboard 的：

```text
今日 / 7 日 / 30 日
按 Run / 模型 / Profile 聚合
Token、延迟、失败、不可用
页面展示
```

都不应成为 Kernel 知识。

内核只处理：

```text
由哪个受信任 Harness 执行验收
验收是否成功
验收证据摘要
证据绑定哪个候选产物
```

业务验收包可以是：

- 固定版本的外部测试制品；
- 外部规划器根据需求生成的测试制品；
- 多 Agent 编排产生并由独立角色审查的验收制品。

无论来源如何，都不进入 Kernel 成为一等概念。

---

# 17. 路由、规划和多 Agent 必须外置

## 17.1 自然语言路由

内核中的路由逻辑只能保留最小安全兜底：

```text
明确批准
明确拒绝
明确状态查询
明确调用已注册能力
无法识别时转交外部 Router
```

内核不得长期承担：

```text
理解自然语言开发需求
从用户话语推断 Contract
选择组件类型
计算业务权限
拆分需求
选择研发角色
```

这些属于外部 Router / Planner Harness。

外部 Router 只能返回声明式建议，不能直接创建真实执行意图、授予权限或修改注册表。

## 17.2 多 Agent

多 Agent 不是 Kernel 原语。

它是外部编排器对多个普通 Run 的组合：

```text
同一模型或 Runtime
+ 不同 Workspace
+ 不同上下文
+ 不同 AGENTS.md
+ 不同权限
+ 不同记忆范围
+ 多个相互引用的 Run
```

内核只需要看到：

```text
多个独立 Run
每个 Run 的身份、范围、快照和权限
输入输出引用
真实调用与回执
```

内核不需要知道：

```text
Planner Agent
Coding Agent
Audit Agent
Deploy Agent
主持人
专家
投票
共识
```

这些角色和协作策略全部属于外部编排。

## 17.3 修复与自我进化编排

观察、诊断、修复、重试、回退策略也应外置：

```text
Observer Harness
→ 发现异常
→ 外部 Diagnoser
→ 产生 Repair Request
→ 外部编排调用 Coding / Audit / Deploy
→ 候选修复
→ 内核治理批准与结果记录
```

内核不得内建：

```text
SelfRepairManager
AutoFixAgent
RepairWorkflow
```

作为产品级模块。

---

# 18. Catalog、Profile 与模板的归属

## 18.1 内核接口定义

内核必须定义并公开自己真实提供的稳定接口：

```text
事件读取
执行意图提交
批准与决定
回执格式
组件启用/禁用/回滚
权限要求
接口版本
```

这是 Kernel 的对外边界，因此接口定义权属于 Kernel。

## 18.2 接口如何被组合

以下内容应外置：

```text
哪些接口适合完成某个需求
示例代码
SDK
项目模板
测试工具
业务验收
如何组合多个接口
```

这些属于 Coding Harness、Planner Harness 和外部开发资料。

## 18.3 组件模板

"长期服务模板""一次性工具模板""上下文处理模板"等，属于外部 Harness。

内核不得内建业务化模板名称，例如：

```text
token-dashboard-v0
memory-service-v0
router-service-v0
```

内核最多关心外部效果的最低治理差异，例如：

```text
一次性调用
长期运行服务
```

即使这一差异，也应尽可能通过受信任 Deployment Harness 的强类型回执表达，而不是让 Kernel 理解服务业务。

---

# 19. 防止 Kernel 膨胀的判定规则

任何新设计进入 Kernel 前，必须回答：

1. 如果完全放在外部 Harness，是否会导致安全边界可被绕过？
2. Kernel 是否必须亲自验证这一事实，而不能相信外部回报？
3. 该概念是否长期稳定，与具体产品和工作方法无关？
4. 是否无法由现有身份、范围、运行、意图、决定、调用、回执和快照组合表达？
5. 是否只是为了让当前实现更方便？

判断规则：

```text
如果只是为了理解需求、规划工作、安排角色、生成代码、
设计测试、选择模型、重试修复或展示产品，
则必须外置。
```

```text
如果只是为了审计某个外部结果，
优先记录不透明摘要或回执，
而不是让 Kernel 理解该外部制品。
```

```text
如果现有治理链可以表达，
禁止新增 Kernel 产品概念。
```

---

# 20. 对当前 Token Dashboard 路线的具体约束

通用长期服务 Harness 只负责：

```text
受限代码生成或加载
隔离编译
沙箱
事件读取
游标
状态保存
只读 HTTP
健康检查
进程恢复
部署与回滚
```

Token Dashboard 的具体业务要求：

```text
Token 字段
时间窗口
按 Run / 模型 / Profile 聚合
延迟与失败
页面布局和显示
```

必须属于外部需求规划与验收包。

因此应明确区分：

```text
通用长期服务 Harness
≠
Token Dashboard 验收包
```

当前第一个 North Star 可以使用一个固定的外部 Token Dashboard 验收包，以确保结果可信。

后续可由外部 Planner 或多 Agent 编排动态产生验收包，但仍不进入 Kernel。

---

# 21. 新的强制 Non-Action List

除非出现经过证明的安全原语缺口，否则不得在 Kernel 中新增：

```text
Plan
DevelopmentPlan
AcceptancePlan
Planner
AgentRole
MultiAgent
Workflow
RepairManager
Dashboard
MemoryStrategy
CompressionStrategy
SchedulerBusinessRule
ModelSelectionStrategy
```

这些名词可以存在于外部 Harness 的 API、制品和文档中，但不是 Kernel 一等概念。

此列表是 §10（Screen Only, Do Not Slim）Non-Action List 的补充约束，并非替代。

---

# 22. 更新后的最终决策 && Final Direction

## 22.1 最终决策

```text
DECISION:
Keep Kernel ignorant of plans and orchestration.
Keep natural-language understanding outside Kernel.
Keep business acceptance outside Kernel.
Keep multi-agent roles and collaboration outside Kernel.
Keep repair strategy outside Kernel.
Kernel may bind opaque artifact digests without understanding artifact semantics.
Kernel publishes stable interfaces; external Harnesses discover and compose them.
Prefer external orchestration over new Kernel concepts.
```

## 22.2 最终边界

> 内核不是研发总监、项目经理、验收专家或多 Agent 协调器。
> 内核只是一个不可绕过的身份、事实、授权、执行、证据和版本治理边界。

## 22.3 最终方向

> Agent Core 的价值不在于内建多少"Agent 功能"，而在于少量不可绕过的事实、授权、快照、执行和证据原语，能够安全组合出不断自我开发、自我观察和自我修复的外部组件生态。
