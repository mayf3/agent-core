# Feishu UPGRADE Full E2E Smoke

> Deployment configuration, contract notes, and smoke checklist for the
> Feishu text‑approval → UPGRADE → Runtime capability invocation flow.

## Reference Commit

```
acf7db55db6a491f8cd0069dabd8554d582f3cdd
```

## Token Env Alignment

### Current env names

| Component | Env var | Where read |
|-----------|---------|------------|
| Kernel | `AGENT_CORE_CAPABILITY_DECISION_TOKEN` | `src/config.rs:136` |
| Feishu Connector | `AGENT_CORE_KERNEL_DECISION_TOKEN` | `connectors/feishu/src/config.ts:45` |

### Requirement

Both env vars **must hold the same value**. This is a deployment‑config
requirement — the two names are not expected to converge in the short term.

When rotating the token:

1. Generate a new token (e.g. `python3 -c "import secrets; print(secrets.token_hex(16).upper())"`).
2. Update **both** launchd plists:
   - `~/Library/LaunchAgents/com.agent-core.kernel.plist`
     → `AGENT_CORE_CAPABILITY_DECISION_TOKEN`
   - `~/Library/LaunchAgents/com.agent-core.feishu-connector.plist`
     → `AGENT_CORE_KERNEL_DECISION_TOKEN`
3. Unload and re‑load the launchd jobs so the new plist is picked up:
   ```bash
   launchctl bootout gui/$(id -u)/<label>
   launchctl bootstrap gui/$(id -u) <plist-path>
   ```
4. If the Kernel is started manually, also update the env/`.env` file.

### Security

- **Never print the raw token** in reports or logs.
- Only display masked tokens: e.g. `DBC4...D250`.
- Consider the token compromised if the raw value appears in any report.
- Do **not** commit token values to any Git branch.

---

## UPGRADE Full E2E Checklist

This checklist validates the complete flow: proposal submission, Feishu text
approval, decision activation, and Runtime tool invocation.

### Pre‑condition

- [ ] **Four ports listening**
  - Port 4130 (Kernel)
  - Port 4131 (Feishu Connector)
  - Port 7200  (Coding Harness)
  - Port 7300  (Capability Host)
- [ ] **Capability Host health** returns `{"status":"ok"}`
- [ ] Decision token is **masked in any written output** (e.g. `DBC4...D250`)

### Proposal

- [ ] Use an **existing** `operation_name` (e.g. `external.calc_smoke_…`).
- [ ] **Do not create** a new `operation_name`.
- [ ] `change_type` must be **UPGRADE** (not CREATE).
- [ ] Proposal status is `PendingApproval`.

If the proposal is CREATE instead of UPGRADE → **STOP** — do not approve.

### Feishu Approval

- [ ] Connector **intercepts** the `批准 <proposal_id>` command.
- [ ] The approval command does **not** enter the LLM.
- [ ] `GET /v1/capability-change-proposals/{proposal_id}` succeeds.
- [ ] `POST …/decision` returns **HTTP 200**.
- [ ] **No 401** — token mismatch resolved.
- [ ] **No HTTP 500** — no UPGRADE registration regression.
- [ ] **No PRIMARY KEY conflict** — idempotency fix (PR #177) holds.
- [ ] New `snapshot` is activated.

### Runtime Tool Invocation

- [ ] The Run uses the **new** (activated) snapshot.
- [ ] Capability Host executes the real artifact.
- [ ] `Receipt.status` = `Succeeded`.
- [ ] No `output_schema_violation`.
- [ ] Output value matches expected result (e.g. `42`).
- [ ] Feishu reply contains the tool result.

---

## Output Schema Contract

### Problem observed

The calculator artifact (`external.calc_smoke_20260707_174929`) returns a **raw
integer** from the Capability Host:

```json
{"ok":true,"protocol_version":"external-harness-v1","result":42}
```

The `result` field extracted by the adapter is the bare integer `42`.

The manifest `output_schema` had previously been set to:

```json
{"type":"object","required":["result","status"],"additionalProperties":false,
 "properties":{"result":{"type":"integer"},"status":{"type":"string"}}}
```

This caused an `output_schema_violation` because the *value of the result field*
(a plain integer) was validated against an object schema.

### Principle

The `manifest.output_schema` **must** describe the actual JSON output of the
artifact — not the envelope, not wrapper fields — because the external‑harness
adapter strips the envelope and validates only the `result` payload.

| If artifact returns … | output_schema should be … |
|-----------------------|---------------------------|
| A raw integer `42` | `{"type":"integer"}` |
| A plain string `"ok"` | `{"type":"string"}` |
| A structured object | `{"type":"object", "required":[…], "properties":{…}}` |

This is **not** a Runtime bug; it is a contract mismatch between the manifest
schema and the artifact's real output.

### Fix procedure

1. Create a new manifest with the corrected `output_schema`.
2. Store it in the content store (`sha256/<prefix>/<subdir>/<full>/object`).
3. Submit an **UPGRADE** proposal referencing the new manifest.
4. Approve via the standard Feishu approval flow.
5. Verify the Runtime tool call now returns `Receipt.status = Succeeded`.

---

## Successful Reference Run

```
operation_name:  external.calc_smoke_20260707_174929
proposal_id:     proposal_83aea92876d84ab080faf57c336bae3a
old snapshot:    snap_7b03e18e…
new snapshot:    snap_805da147…

Runtime add(20,22):
    Receipt.status = Succeeded
    output = 42

Runtime multiply(6,7):
    Receipt.status = Succeeded
    output = 42

Feishu replies (masked message IDs):
    "工具调用成功，返回结果为：**42**。即 add(20,22) = 42。"
    "工具调用成功，返回结果为：**42**。即 multiply(6,7) = 42。"
```

---

## Common Failure Modes

| Symptom | Likely root cause | Action |
|---------|-------------------|--------|
| `401` / token mismatch | Kernel and Connector decision tokens differ. | Align env vars; reload launchd jobs. |
| `HTTP 500` / `PRIMARY KEY conflict` | UPGRADE manifest registration idempotency regression (PR #177). | Investigate regression in `handle_decision` / `register_harness_manifest`. |
| `output_schema_violation` | Manifest `output_schema` does not match artifact's real output. | Fix schema via new UPGRADE proposal (see § output_schema contract). |
| Proposal is **CREATE** not UPGRADE | Request did not re‑use an existing `operation_name`. | **STOP** — do not approve. Fix the submission logic. |
| Approval command **enters LLM** | Connector did not intercept it. | Check `parseApprovalCommand` regex; verify Connector is running and WS connected. |
| `GET proposal` returns `not_found` | Proposal ID is wrong or expired. | Verify `proposal_id` is correct; check `expires_at`. |
| `snapshot conflict` / `stale_expected_snapshot` | Another upgrade activated a different snapshot between proposal and decision. | Re‑submit the proposal (it will use the current active snapshot). |

---

## References

- Kernel decision‑token env: `src/config.rs` line 136
- Feishu Connector decision‑token env: `connectors/feishu/src/config.ts` line 45
- Approval interception: `connectors/feishu/src/index.ts`
- Approval execution: `connectors/feishu/src/approval.ts`
- Handle decision: `src/server/capability_routes.rs` lines 200–411
- Schema validation: `src/adapters/external_harness.rs` lines 220–235
- UPGRADE decision idempotency fix: PR #177
- HTTP hook client: PR #181
