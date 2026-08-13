# External Component Self-Deployment V0

Status: **implemented + locally proven** (`EXTERNAL_COMPONENT_SELF_DEPLOYMENT_V0_PASS`)

Goal: let the main Agent self-deploy **Kernel-external** components via two ordinary
`external.*` Harness operations. The Kernel/Runtime code is **not touched** (zero
diff); the Kernel never gains a self-deploy capability; the Deployment Harness
frozen core is **not modified** (zero diff, still a pure immutable-artifact CAS
consumer).

## Boundary principles

| Layer | Responsibility | Never |
|---|---|---|
| **Kernel / Runtime** | Dispatch registered `external.*` operations through the generic external-harness transport; approve via grants + pinned snapshot | No component allowlist, no component-name knowledge, no deploy orchestration, no `deploy_kernel` |
| **Coding/Build Harness** (`tools/coding-harness`) | `external.component_build`: allowlist (Infra config), immutable-commit lineage check, fixed build recipe, produce immutable artifact + manifest into the shared CAS | No Agent-supplied command/path/env |
| **Deployment Harness** (`tools/deployment-harness`) | Pure CAS consumer: validate immutable input → deploy → health → rollback → Receipt (`deployment.effect.v0`) | No build, no source knowledge, no code change |
| **component-deploy-adapter** (independent userspace process) | `external.component_deploy`: translate `external-harness-v1` Invocation → the Deployment Harness's unchanged `POST /v1/deployments` | No Kernel access; never part of the Deployment Harness frozen core |

Config starts components (listen, credentials, build mapping, allowlist); the
Registry selects which Harness operations are active. The `component_id →
artifact` mapping is Infra config at the Build Harness, **never** the Kernel
Registry.

## The two operations

### `external.component_build({component_id, source_commit})`
Bound to the Coding/Build Harness (`127.0.0.1:7200`). Inside the harness:

1. **Allowlist, fail-closed** (`BuildConfig` from `CODING_CONFIG.component_builds`).
   An id absent from the map is rejected (`component_not_allowed`). This is the
   single place unsupported components — including any kernel component — are
   refused. The Kernel never sees or enforces this list.
2. **Commit lineage policy** (stricter than "object exists"):
   - `source_commit` must be a full 40-hex SHA (`source_commit_not_immutable`);
   - `git rev-parse --verify <commit>^{commit}` must resolve to the exact input
     (`source_commit_not_canonical` if it is a ref/short-sha);
   - `git merge-base --is-ancestor <commit> <allowed_ref>` must pass
     (`source_commit_not_in_lineage`) — the commit must be on the authoritative
     source's allowed main line, never a detached/grafted commit.
3. **Authoritative source at the commit**: template files + projection module are
   read via `git show <commit>:<path>` (never the working tree).
4. **Fixed build**: materialize the `generated-hook-consumer` crate from the
   `hook-consumer-service` template + the generator's known-good projection
   (`KNOWN_GOOD_TOKEN_SOURCE`), then a fixed `cargo build --release`. The Agent
   supplies no command/path/env.
5. **Immutable artifact**: the release binary is content-addressed
   (`artifact_digest` covers the whole running artifact — application code +
   compiled dependencies). The OS/toolchain is an Infra runtime dependency,
   outside the artifact identity.
6. **Immutable manifest**: a `ServiceManifest` (`HookConsumerService`,
   `managed-service-v0`, `event.observe.v0`/`journal.observe`, loopback, GET
   `/health`) is content-addressed (`manifest_digest`).
7. Receipt `result = {component_id, source_commit, artifact_digest,
   manifest_digest, version, evidence}`.

### `external.component_deploy({component_id, artifact_digest, manifest_digest})`
Bound to the **independent userspace adapter** `component-deploy-adapter`
(second binary in the deployment-harness crate; `main.rs`/`server.rs`/
`manager.rs`/`process.rs`/`config.rs` are untouched). The adapter:

1. loads the immutable `ServiceManifest` from the shared CAS by
   `manifest_digest` (→ version, component_id; cross-checks `artifact_digest`
   and `component_id` against the request);
2. synthesizes a deterministic, self-validating `DeploymentIntent`
   (governance ids are a pure function of the immutable inputs, so re-deploys
   replay at the Deployment Harness);
3. POSTs to the Deployment Harness's **unchanged** `POST /v1/deployments`;
4. wraps the bare `DeploymentReceipt` into
   `{protocol_version:"external-harness-v1", ok:true, result:{component_id,
   deployment_id, artifact_digest, manifest_digest, version, status,
   health_status, endpoint, rollback_ref, replayed}}` (errors →
   `{ok:false, error_code}` on HTTP 200).

The Agent reads the build Receipt and itself initiates the deploy invocation —
no commit→digest resolver map connects the two steps.

## Deployment vs Registry activation (kept separate)

- A deploy only **runs a new version** of an external process. Neither operation
  contacts the Kernel; the `RegistrySnapshot` is untouched (provable: the build
  op uses git+cargo+CAS; the adapter/deployment-harness path has no Kernel
  DB/journal client and no journal dependency — see
  `tools/deployment-harness/Cargo.toml`).
- Activating a Harness as a Runtime-invocable operation remains the existing
  separate seam: `POST /v1/harness/register` + `POST /v1/harness/enable` (and
  `POST /v1/hook-bindings/bind` for hook contracts). Deploying a new version
  never silently changes Registry/HookBinding state.
- The Connector is a deployed edge process and has **no** activation requirement.

## Stop / status / rollback reuse boundary (PR #234 scope)

- Component `status` / `disable` / `rollback` all **reuse the Deployment Harness
  existing lifecycle**: `GET /v1/components/{id}` (status), `POST
  /v1/components/{id}/disable` (stop the deployed process), `POST
  /v1/components/{id}/rollback`. These routes live in the Deployment Harness
  frozen core and are unchanged by this milestone.
- PR #234 adds **only** the Agent-facing `external.component_build` +
  `external.component_deploy` operations. It does **not** re-wrap or duplicate
  stop/status; there is no Agent-facing `component_stop` / `component_status`
  operation in this milestone.
- `disable` means stopping a **deployed external component process** (the
  Deployment Harness disable effect) — it is **not** the Feishu `/stop` Run
  control (run-session termination stays with ChatControl and is unrelated to
  this lifecycle).
- Deployment `status` likewise means the component deployment state
  (`healthy` / `disabled` / `rolled_back` + `health_status`) — it is **not** the
  Feishu `/status` agent-state query.

## Registration + grant (production, zero Kernel change)

1. Register both operations as ordinary `HarnessManifest`s
   (`operation_name` = `external.component_build` / `external.component_deploy`,
   `protocol_version` = `external-harness-v1`, loopback endpoints):
   `POST /v1/harness/register` (IPC bearer).
2. Enable them into the active snapshot: `POST /v1/harness/enable`.
   (Data-driven; `coding_operation_risk` defaults unknown names to `Write`.)
3. **Grant to the Feishu owner via the existing trusted capability activation
   lifecycle** (the only zero-code-diff producer of `external_operation_grants`
   rows — `activate_trusted_capability_atomic` →
   `create_external_operation_grant_tx`, channel=Feishu, p2p,
   scope=principal_channel): drive the Coding Harness acceptance pipeline for
   each operation, then approve via
   `POST /v1/capability-change-proposals/{id}/decision`. **No new Kernel grant
   route.**
4. Thereafter the owner can invoke both operations in a Feishu p2p session; the
   generic transport authenticates them (`AGENT_CORE_CAPABILITY_HOST_EXECUTION_TOKEN`).

## Canary (proven locally)

`tools/coding-harness/tests/component_self_deploy_canary.rs` (ignored test; run
with `--ignored`). Chain proven:

```
real commit (HEAD)
 -> external.component_build  -> immutable artifact_digest + manifest_digest (CAS)
 -> external.component_deploy (adapter) -> UNCHANGED Deployment Harness
 -> real generated-hook-consumer process installed + started under env_clear
 -> component polls a real event.observe.v0 stub -> ready
 -> GET /health -> 200 + X-Agent-Core-Component/Version/Instance
 -> Receipt: status=healthy, health_status=ready, rollback_ref=null
 -> identical re-deploy -> replayed=true, same deployment_id   (replay idempotency)
 -> upgrade v0.2.0 -> rollback_ref = previous artifact_digest  (rollback reference)
```

The Deployment Harness binary core is **not** exercised with new code paths —
the adapter calls its pre-existing, tested `/v1/deployments` route.

## File map (this milestone)

- `tools/coding-harness/src/component_build.rs` — the build operation + allowlist
  + lineage checks + materialization + manifest.
- `tools/coding-harness/src/server.rs` — dispatch arm + execution-token auth for
  `external.component_build`.
- `tools/coding-harness/src/config.rs` — `component_builds` allowlist config.
- `tools/deployment-harness/src/component_deploy_adapter.rs` — the independent
  adapter (translation + HTTP server + tests).
- `tools/deployment-harness/src/bin/component_deploy_adapter.rs` — adapter binary
  entry; `Cargo.toml` `[[bin]]`; `lib.rs` module line.
- `tools/coding-harness/tests/component_self_deploy_canary.rs` — the end-to-end
  canary (ignored test).

Kernel (`src/`) and Runtime (`agent-runtime/`) diffs: **none from this
milestone.**

## Deferred (explicitly out of scope)

- **Feishu Connector self-deploy**: the frozen `deployment.service-manifest.v0`
  contract accepts only `HookConsumerService` components with
  `event.observe.v0`/`journal.observe`; the Connector is an ingress/execute edge
  adapter that is architecturally incompatible (kind/contracts/permissions,
  identity-header health probe, `env_clear` runtime-env profile). Supporting it
  requires a Kernel lifecycle-contract extension — a product-semantic change,
  disallowed under frozen v0. Reported as the minimal protocol blocker.
- Harness deploy + activation canary beyond the connector-deferred scope.
- Portable self-contained JS bundling (not needed — the canary target is a
  single self-contained Rust binary).
