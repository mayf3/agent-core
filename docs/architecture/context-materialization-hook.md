# Pre-model Context Hook boundary

Status: frozen architecture decision
Baseline: `41f9df1a8f4eaab001881f5d53ce053de648604b`

## One allowed path

Every model invocation, including the initial invocation and every tool
follow-up, uses the same path:

```text
Model Adapter stages an opaque CandidateInput artifact
→ Kernel binds its reference to Run, Session, scope, and immutable refs
→ Kernel calls the authorized context.prepare.v0 provider
→ Provider returns one Context Artifact or ordered opaque artifact refs
→ Kernel verifies identity, correlation, bindings, and digests
→ Model Adapter materializes the complete LlmInput and hard-budget receipt
→ Kernel permits or rejects the model call
```

The name `context.prepare.v0` is retained as the existing lifecycle identifier.
It no longer means fragment injection and it does not define a compression
method.

## Kernel authority

The Kernel owns only:

- the single pre-model hook point;
- the configured Provider identity, request authorization, and authenticated
  response proof;
- request correlation;
- Run, Session, and scope binding;
- Candidate, Context Artifact, and immutable-ref digests;
- enforcement of the Model Adapter hard-budget result;
- Journal facts and hook/model receipts;
- explicit configured behavior when the Provider fails;
- transport timeout and request/response size limits.

The Kernel treats candidate and context artifact bytes as opaque. It does not
parse them to select, truncate, summarize, replace, or reorder context.

## External Provider authority

The Provider owns all context policy, including history selection, retrieval,
summarization, truncation, replacement, retention, ordering, and any preview.
Policy changes require no Kernel change.

The Provider response contains no Provider identity field. Identity is assigned
by the trusted endpoint binding and proven with the binding credential.

## Model Adapter authority

The selected Model Adapter owns:

- Candidate and Context Artifact media format;
- ordered-artifact materialization;
- the final `LlmInput`;
- message structure and tool schemas;
- tool-call/tool-result wire validity;
- tokenizer and protocol overhead;
- reserved output;
- the final hard-budget result.

An `OverBudget` result is terminal for that attempted invocation. The Kernel
records the refusal and must not call the model.

## Failure behavior

The configured behavior is explicit:

- `fail_closed`: record the failed hook receipt and terminate the attempted
  model path;
- `fail_open` or `degrade`: record the failed hook receipt and pass the
  original candidate artifact to the Model Adapter;
- disabled or unconfigured: use the candidate artifact without inventing a
  context policy.

There is no Kernel compaction fallback.

## Persistence

Context materialization changes only the model view. Source Journal events,
tool Receipts, and their original payloads remain append-only and unchanged.
Hook receipts record only bounded governance metadata and digests, never
artifact contents or credentials.

## Negative boundary

Kernel code must not contain:

- a ContextPlan DSL or plan interpreter;
- keep/drop/truncate/replace/summarize actions;
- context-history selection or preview rules;
- ContextBlockKind-based compression behavior;
- a Kernel context threshold or token estimator;
- Provider identity accepted from response JSON;
- unverified plan or artifact digests;
- fabricated event boundaries or empty scope/source bindings;
- an advisory-only over-budget branch;
- context jobs, attempts, repair, cache epochs, or a new state machine;
- Provider process management or Capture LLM test utilities.

The real Provider process, Capture LLM, E2E orchestration, and acceptance
statistics live in the independent `tools/context-hook-harness` package. That
package is deliberately absent from the root Cargo workspace.

## Acceptance

The candidate is acceptable only when evidence proves:

1. initial and follow-up calls use the same hook entry;
2. Provider identity and authorization come from the actual binding;
3. Run, Session, scope, Candidate, Context Artifact, and immutable refs are
   verified;
4. the Model Adapter materializes the complete model input and hard budget;
5. an over-budget result prevents the model call;
6. Provider failure degrades or terminates exactly as configured;
7. source Journal and Receipt payloads remain intact;
8. a real external Provider changes the model input, after which a tool call,
   assistant reply delivery, and Run completion still succeed.
