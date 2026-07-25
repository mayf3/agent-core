import { describe, it, mock } from "node:test";
import assert from "node:assert";
import {
  parseApprovalCommand,
  isApprovalAuthorised,
  fetchProposal,
  executeApprovalCommand,
  handleProposalCardAction,
  parsePendingProposalPresentation,
  parseProposalCardAction,
  sendPendingProposalCardReply,
  ApprovalError,
  type ApprovalConfig,
} from "./approval.js";
import { validateExecute } from "./execute-server.js";

const ownerOpenId = "ou_owner123";
const makeConfig = (overrides?: Partial<ApprovalConfig>): ApprovalConfig => ({
  kernelBaseUrl: "http://127.0.0.1:4130",
  decisionToken: "test-decision-token",
  ownerOpenId,
  ...overrides,
});
const executeAsOwner = (config: ApprovalConfig, command: Parameters<typeof executeApprovalCommand>[1]) =>
  executeApprovalCommand(config, command, `feishu:open_id:${ownerOpenId}`);
const approvalBinding = () => ({
  approval_id: "approval_abc",
  principal_id: `feishu:open_id:${ownerOpenId}`,
  expected_source_snapshot_id: "snap_old",
  candidate_digest: "sha256:candidate",
  artifact_digest: "sha256:abc",
  manifest_digest: "sha256:def",
  decision_nonce: "nonce_abc",
  expires_at: "2026-07-15T00:00:00Z",
  status: "Pending",
  origin_channel: "Feishu",
  origin_conversation_kind: "p2p",
});

// ═══════════════════════════════════════════════════════════════════
// Approval parser tests
// ═══════════════════════════════════════════════════════════════════

describe("parseApprovalCommand", () => {
  it("approve: 批准 proposal_abc123", () => {
    assert.deepStrictEqual(parseApprovalCommand("批准 proposal_abc123"), {
      kind: "approve", proposalId: "proposal_abc123", reason: "",
    });
  });
  it("approve: trim whitespace", () => {
    assert.deepStrictEqual(parseApprovalCommand("  批准 proposal_x  "), {
      kind: "approve", proposalId: "proposal_x", reason: "",
    });
  });
  it("reject: 拒绝 proposal_abc123 need more testing", () => {
    assert.deepStrictEqual(parseApprovalCommand("拒绝 proposal_abc123 need more testing"), {
      kind: "reject", proposalId: "proposal_abc123", reason: "need more testing",
    });
  });
  it("reject: multi-word Chinese reason", () => {
    const cmd = parseApprovalCommand("拒绝 proposal_x 功能未完整，需要补测试");
    assert.strictEqual(cmd?.kind, "reject");
    assert.ok(cmd?.reason.includes("功能"));
  });
  it("non-command: ordinary text returns null", () => {
    assert.strictEqual(parseApprovalCommand("6 * 7 等于多少？"), null);
  });
  it("non-command: empty string returns null", () => {
    assert.strictEqual(parseApprovalCommand(""), null);
  });
  it("non-command: '批准' without proposal_id returns null", () => {
    assert.strictEqual(parseApprovalCommand("批准"), null);
  });
});

// ═══════════════════════════════════════════════════════════════════
// Authorisation tests
// ═══════════════════════════════════════════════════════════════════

describe("isApprovalAuthorised", () => {
  it("owner + p2p → allowed", () => {
    assert.strictEqual(isApprovalAuthorised(makeConfig(), "p2p", ownerOpenId), null);
  });
  it("non-owner + p2p → rejected", () => {
    const err = isApprovalAuthorised(makeConfig(), "p2p", "ou_stranger");
    assert.ok(err?.includes("不是审批人"));
  });
  it("owner + group → rejected", () => {
    const err = isApprovalAuthorised(makeConfig(), "group", ownerOpenId);
    assert.ok(err?.includes("仅支持私聊"));
  });
  it("missing decision token → rejected", () => {
    const err = isApprovalAuthorised(makeConfig({ decisionToken: undefined }), "p2p", ownerOpenId);
    assert.ok(err?.includes("未配置"));
  });
  it("missing owner → rejected", () => {
    const err = isApprovalAuthorised(makeConfig({ ownerOpenId: undefined }), "p2p", ownerOpenId);
    assert.ok(err?.includes("未配置"));
  });
});

// ═══════════════════════════════════════════════════════════════════
// fetchProposal tests
// ═══════════════════════════════════════════════════════════════════

describe("fetchProposal", () => {
  it("returns proposal info on success", async () => {
    const config = makeConfig();
    const mockFetch = mock.fn(() =>
      Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve({
          proposal_id: "proposal_abc",
          status: "PendingApproval",
          operation_name: "external.calculator",
          manifest_id: "manifest_xyz",
          artifact_digest: "sha256:abc",
          manifest_digest: "sha256:def",
          endpoint: "http://127.0.0.1:7300/execute",
          approval: approvalBinding(),
        }),
      }),
    );
    (globalThis as any).fetch = mockFetch;
    const info = await fetchProposal(config, "proposal_abc");
    assert.strictEqual(info.proposal_id, "proposal_abc");
    assert.strictEqual(info.status, "PendingApproval");
    assert.strictEqual(info.artifact_digest, "sha256:abc");
    assert.strictEqual(info.manifest_digest, "sha256:def");
    assert.strictEqual(info.manifest_id, "manifest_xyz");
  });

  it("throws ApprovalError on 404", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ error: "proposal_not_found" }) }),
    );
    await assert.rejects(
      () => fetchProposal(config, "proposal_nonexistent"),
      (err: any) => err instanceof ApprovalError && err.code === "proposal_not_found",
    );
  });

  it("throws ApprovalError on unauthorized", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: false, status: 401, json: () => Promise.resolve({ error: "unauthorized" }) }),
    );
    await assert.rejects(
      () => fetchProposal(config, "proposal_abc"),
      (err: any) => err instanceof ApprovalError && err.code === "unauthorized",
    );
  });

  it("fails closed for a group-origin or different-owner approval", async () => {
    for (const approval of [
      { ...approvalBinding(), origin_conversation_kind: "group" },
      { ...approvalBinding(), principal_id: "feishu:open_id:someone_else" },
    ]) {
      (globalThis as any).fetch = mock.fn(() => Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve({
          proposal_id: "proposal_abc",
          artifact_digest: "sha256:abc",
          manifest_digest: "sha256:def",
          approval,
        }),
      }));
      await assert.rejects(
        () => fetchProposal(makeConfig(), "proposal_abc"),
        (err: any) => err instanceof ApprovalError &&
          ["invalid_approval_origin", "invalid_approval_binding"].includes(err.code),
      );
    }
  });
});

// ═══════════════════════════════════════════════════════════════════
// executeApprovalCommand (orchestration) tests
// ═══════════════════════════════════════════════════════════════════

describe("executeApprovalCommand", () => {
  const mockProposalJson = () => ({
    proposal_id: "proposal_abc",
    status: "PendingApproval",
    operation_name: "external.calculator",
    manifest_id: "manifest_xyz",
    artifact_digest: "sha256:abc",
    manifest_digest: "sha256:def",
    endpoint: "http://127.0.0.1:7300/execute",
    approval: approvalBinding(),
  });

  it("approve forwards the complete authoritative binding", async () => {
    const config = makeConfig();
    let callCount = 0;
    (globalThis as any).fetch = mock.fn(() => {
      callCount++;
      if (callCount === 1) {
        // GET proposal
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(mockProposalJson()) });
      }
      // POST decision
      return Promise.resolve({
        ok: true, status: 200,
        json: () => Promise.resolve({
          approval_id: "approval_abc",
          activated_snapshot_id: "snap_new",
          decision_id: "decision_abc",
          host_deployment_id: "deployment_abc",
          status: "Activated",
        }),
      });
    });
    const result = await executeAsOwner(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(result.ok);
    assert.ok(result.replyText.includes("proposal_abc"));
    assert.ok(result.replyText.includes("decision_abc"));
    assert.ok(result.replyText.includes("snap_new"));
    assert.strictEqual(callCount, 2);
    const post = (globalThis.fetch as any).mock.calls[1].arguments[1];
    const body = JSON.parse(post.body);
    assert.deepStrictEqual(body, {
      decision: "approved",
      approval_id: "approval_abc",
      decision_nonce: "nonce_abc",
      principal_id: `feishu:open_id:${ownerOpenId}`,
      expected_source_snapshot_id: "snap_old",
      candidate_digest: "sha256:candidate",
      artifact_digest: "sha256:abc",
      manifest_digest: "sha256:def",
    });
  });

  it("reject succeeds with real digest", async () => {
    const config = makeConfig();
    let callCount = 0;
    (globalThis as any).fetch = mock.fn(() => {
      callCount++;
      if (callCount === 1) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(mockProposalJson()) });
      }
      return Promise.resolve({
        ok: true, status: 200,
        json: () => Promise.resolve({
          approval_id: "approval_abc", decision_id: "decision_reject", status: "Rejected",
        }),
      });
    });
    const result = await executeAsOwner(config, { kind: "reject", proposalId: "proposal_abc", reason: "need more testing" });
    assert.ok(result.ok);
    assert.ok(result.replyText.includes("拒绝"));
    assert.ok(result.replyText.includes("need more testing"));
    assert.strictEqual(callCount, 2);
  });

  it("fails with digest_mismatch error", async () => {
    const config = makeConfig();
    let callCount = 0;
    (globalThis as any).fetch = mock.fn(() => {
      callCount++;
      if (callCount === 1) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(mockProposalJson()) });
      }
      return Promise.resolve({
        ok: false, status: 400,
        json: () => Promise.resolve({ error: "digest_mismatch" }),
      });
    });
    const result = await executeAsOwner(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("digest_mismatch") || result.replyText.includes("失败"));
  });

  it("fails when proposal is expired", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({ ...mockProposalJson(), status: "Expired" }) }),
    );
    const result = await executeAsOwner(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("Expired"));
  });

  it("forwards an identical terminal callback for Kernel replay", async () => {
    let calls = 0;
    (globalThis as any).fetch = mock.fn(() => {
      calls += 1;
      if (calls === 1) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({
          ...mockProposalJson(), status: "Activated",
          approval: { ...approvalBinding(), status: "Approved" },
        }) });
      }
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({
        approval_id: "approval_abc", decision_id: "decision_abc", status: "Activated",
        activated_snapshot_id: "snap_new", host_deployment_id: "deployment_abc",
      }) });
    });
    const result = await executeAsOwner(
      makeConfig(), { kind: "approve", proposalId: "proposal_abc", reason: "" },
    );
    assert.equal(result.ok, true);
    assert.equal(calls, 2, "terminal callback must reach Kernel replay handling");
  });

  it("fails with network error", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: false, status: 500, json: () => Promise.resolve({ error: "internal_error" }) }),
    );
    const result = await executeAsOwner(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(!result.ok);
  });

  it("fails with proposal_not_found", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ error: "proposal_not_found" }) }),
    );
    const result = await executeAsOwner(config, { kind: "approve", proposalId: "proposal_nonexistent", reason: "" });
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("不存在") || result.replyText.includes("not_found"));
  });

  it("fails closed when GET omits the approval binding", async () => {
    const config = makeConfig();
    const proposal = mockProposalJson() as any;
    delete proposal.approval;
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(proposal) }),
    );
    const result = await executeAsOwner(config, {
      kind: "approve", proposalId: "proposal_abc", reason: "",
    });
    assert.equal(result.ok, false);
    assert.match(result.replyText, /审批绑定/);
  });

  it("fails closed when GET returns a different proposal identity", async () => {
    (globalThis as any).fetch = mock.fn(() => Promise.resolve({
      ok: true, status: 200,
      json: () => Promise.resolve({ ...mockProposalJson(), proposal_id: "proposal_other" }),
    }));
    const result = await executeAsOwner(makeConfig(), {
      kind: "approve", proposalId: "proposal_abc", reason: "",
    });
    assert.equal(result.ok, false);
    assert.match(result.replyText, /格式异常/);
  });

  it("fails closed when Kernel returns a decision status for the other action", async () => {
    let calls = 0;
    (globalThis as any).fetch = mock.fn(() => {
      calls += 1;
      if (calls === 1) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(mockProposalJson()) });
      }
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({
        approval_id: "approval_abc", decision_id: "decision_wrong", status: "Activated",
      }) });
    });
    const result = await executeAsOwner(makeConfig(), {
      kind: "reject", proposalId: "proposal_abc", reason: "no",
    });
    assert.equal(result.ok, false);
    assert.match(result.replyText, /格式异常/);
  });

  it("renders a durable activation failure as failure, not APPROVED", async () => {
    let calls = 0;
    (globalThis as any).fetch = mock.fn(() => {
      calls += 1;
      if (calls === 1) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(mockProposalJson()) });
      }
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({
        approval_id: "approval_abc", decision_id: "decision_failed",
        status: "ActivationFailed", activation_error: "CAPABILITY_HOST_DEPLOY_FAILED",
      }) });
    });
    const result = await executeAsOwner(makeConfig(), {
      kind: "approve", proposalId: "proposal_abc", reason: "",
    });
    assert.equal(result.ok, false);
    assert.match(result.replyText, /激活失败/);
    assert.doesNotMatch(result.replyText, /APPROVED/);
  });
});

function cardProposal() {
  return {
    proposal_id: "proposal_abc",
    status: "PendingApproval",
    operation_name: "external.calculator",
    manifest_id: "manifest_abc",
    artifact_digest: "sha256:abc",
    manifest_digest: "sha256:def",
    approval: approvalBinding(),
  };
}

function cardAction(overrides: Record<string, unknown> = {}) {
  return {
    operator: { open_id: ownerOpenId },
    action: { value: {
      proposal_id: "proposal_abc",
      approval_id: "approval_abc",
      decision_nonce: "nonce_abc",
      decision: "approved",
      ...overrides,
    } },
  };
}

describe("pending proposal card", () => {
  it("accepts only the fixed structured execute presentation", () => {
    const presentation = {
      kind: "capability_proposal_pending_v1" as const,
      proposal_id: "proposal_abc",
    };
    assert.deepStrictEqual(parsePendingProposalPresentation(presentation), presentation);
    assert.equal(parsePendingProposalPresentation({ kind: "html", proposal_id: "p" }), null);
    const base = {
      protocol_version: "v1", operation: "feishu.send_message",
      invocation_id: "inv_1", decision_id: "dec_1", idempotency_key: "idem_1",
      arguments: { message_id: "om_1", presentation },
    };
    assert.doesNotThrow(() => validateExecute(base));
    assert.throws(
      () => validateExecute({ ...base, arguments: { ...base.arguments, text: "ambiguous" } }),
      /invalid execute payload/,
    );
    assert.throws(
      () => validateExecute({ ...base, arguments: { message_id: "om_1", text: "fallback", presentation: { kind: "invalid" } } }),
      /invalid execute payload/,
    );
  });

  it("renders the same identity-bound controls for a managed component", async () => {
    (globalThis as any).fetch = mock.fn(() => Promise.resolve({
      ok: true, status: 200,
      json: () => Promise.resolve({ ...cardProposal(), operation_name: "external.other" }),
    }));
    const requests: any[] = [];
    await sendPendingProposalCardReply(
      { request: async (request: any) => {
        requests.push(request);
        return { data: { message_id: "om_service" } };
      } },
      "om_1",
      { kind: "capability_proposal_pending_v1", proposal_id: "proposal_abc" },
      makeConfig(),
    );
    assert.match(requests[0].data.content, /external\.other/);
  });

  it("GETs authoritative binding and replies with an interactive card", async () => {
    (globalThis as any).fetch = mock.fn(() => Promise.resolve({
      ok: true, status: 200, json: () => Promise.resolve(cardProposal()),
    }));
    const requests: any[] = [];
    const client = { async request(request: any) {
      requests.push(request);
      return { data: { message_id: "om_card" } };
    } };
    const receipt = await sendPendingProposalCardReply(
      client, "om_source",
      { kind: "capability_proposal_pending_v1", proposal_id: "proposal_abc" },
      makeConfig(),
    );
    assert.equal(receipt.message_id, "om_card");
    assert.equal(requests[0].data.msg_type, "interactive");
    assert.match(requests[0].data.content, /external\.calculator/);
    assert.match(requests[0].data.content, /approval_abc/);
  });

  it("normalizes identity-bound card actions", () => {
    assert.deepStrictEqual(parseProposalCardAction({ event: cardAction() }), {
      proposalId: "proposal_abc", approvalId: "approval_abc",
      decisionNonce: "nonce_abc", decision: "approved", operatorOpenId: ownerOpenId,
    });
    assert.equal(parseProposalCardAction(cardAction({ decision: "maybe" })), null);
    assert.equal(parseProposalCardAction({ action: cardAction().action }), null);
  });

  it("binds operator and returns APPROVED with Decision ID and Snapshot", async () => {
    (globalThis as any).fetch = mock.fn((_url: string, init: any) => {
      if (init.method === "GET") {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(cardProposal()) });
      }
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({
        approval_id: "approval_abc", decision_id: "decision_abc", status: "Activated",
        activated_snapshot_id: "snapshot_new", host_deployment_id: "deployment_abc",
      }) });
    });
    const response = await handleProposalCardAction(makeConfig(), cardAction());
    const raw = JSON.stringify(response);
    assert.match(raw, /APPROVED/);
    assert.match(raw, /decision_abc/);
    assert.match(raw, /snapshot_new/);
    const post = (globalThis.fetch as any).mock.calls[1].arguments[1];
    assert.equal(JSON.parse(post.body).principal_id, `feishu:open_id:${ownerOpenId}`);
  });

  it("rejects non-owner and stale card binding before decision POST", async () => {
    const fetchMock = mock.fn(() => Promise.resolve({
      ok: true, status: 200, json: () => Promise.resolve(cardProposal()),
    }));
    (globalThis as any).fetch = fetchMock;
    const stranger = cardAction();
    stranger.operator.open_id = "ou_stranger";
    assert.match(JSON.stringify(await handleProposalCardAction(makeConfig(), stranger)), /不是审批人/);
    assert.equal(fetchMock.mock.callCount(), 0);
    assert.match(
      JSON.stringify(await handleProposalCardAction(makeConfig(), cardAction({ decision_nonce: "old" }))),
      /nonce/,
    );
    assert.equal(fetchMock.mock.callCount(), 1);
  });

  it("tampered_approval_digest_fails_connector_binding", async () => {
    // The GET response returns mismatched approval/proposal digests
    const mismatched = {
      ...cardProposal(),
      artifact_digest: "sha256:proposal_artifact",
      manifest_digest: "sha256:proposal_manifest",
      approval: { ...approvalBinding(), artifact_digest: "sha256:wrong_artifact", manifest_digest: "sha256:wrong_manifest" },
    };
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(mismatched) }),
    );
    const result = await handleProposalCardAction(makeConfig(), cardAction());
    assert.match(JSON.stringify(result), /审批绑定/);
  });

  it("tampered_approval_id_fails_connector_binding", async () => {
    // The card has a different approval_id than the proposal's current approval
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(cardProposal()) }),
    );
    const staleCard = cardAction({ approval_id: "approval_stale" });
    const result = await handleProposalCardAction(makeConfig(), staleCard);
    assert.match(JSON.stringify(result), /卡片已失效|卡片 nonce|approval_id/);
  });

  it("production_handler_sends_bound_decision", async () => {
    // Full path: get proposal → verify binding → POST decision with bound fields
    let callCount = 0;
    (globalThis as any).fetch = mock.fn(() => {
      callCount++;
      if (callCount === 1) {
        return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(cardProposal()) });
      }
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({
        approval_id: "approval_abc", decision_id: "decision_abc", status: "Activated",
        activated_snapshot_id: "snapshot_new", host_deployment_id: "deployment_abc",
      }) });
    });
    const response = await handleProposalCardAction(makeConfig(), cardAction());
    assert.equal(callCount, 2, "must call GET proposal then POST decision");
    assert.match(JSON.stringify(response), /APPROVED/);
    // Verify the POST body carries the authoritative binding fields from GET, not from the card
    const postArgs = (globalThis.fetch as any).mock.calls[1].arguments[1];
    const postBody = JSON.parse(postArgs.body);
    assert.equal(postBody.approval_id, "approval_abc");
    assert.equal(postBody.decision_nonce, "nonce_abc");
    assert.equal(postBody.principal_id, `feishu:open_id:${ownerOpenId}`);
    assert.equal(postBody.artifact_digest, "sha256:abc");
    assert.equal(postBody.manifest_digest, "sha256:def");
    assert.equal(postBody.candidate_digest, "sha256:candidate");
    assert.equal(postBody.expected_source_snapshot_id, "snap_old");
  });
});
