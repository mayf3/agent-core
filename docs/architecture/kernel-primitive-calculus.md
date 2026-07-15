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
Principal  ≈ Identity(K1) + grant set derived from Snapshot(K3,K8)
Session    ≈ Scope(K2) + ordered Journal Events(K6) / Runs(K4)
Run        ≈ K4 (primitive) pinned to Snapshot(K3) under Identity(K1) in Scope(K2)
Registry   ≈ Snapshot(K3) catalogue + Allow Boundary(K8) for tool visibility
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
Capability ≈ Snapshot(K3) operation row + Allow Boundary(K8) + per-principal grant
External Operation ≈ Snapshot(K3) row with BindingKind::External + grant(K8)
Workspace/Profile ≈ Identity(K1) + on-disk profile + Snapshot(K3) of grants (no first-class type today)
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
