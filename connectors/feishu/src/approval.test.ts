import { describe, it, mock } from "node:test";
import assert from "node:assert";
import {
  parseApprovalCommand,
  isApprovalAuthorised,
  fetchProposal,
  executeApprovalCommand,
  ApprovalError,
  type ApprovalConfig,
} from "./approval.js";

const ownerOpenId = "ou_owner123";
const makeConfig = (overrides?: Partial<ApprovalConfig>): ApprovalConfig => ({
  kernelBaseUrl: "http://127.0.0.1:4130",
  decisionToken: "test-decision-token",
  ownerOpenId,
  ...overrides,
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
  });

  it("approve succeeds with real digest", async () => {
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
        json: () => Promise.resolve({ activated_snapshot_id: "snap_new", status: "Activated" }),
      });
    });
    const result = await executeApprovalCommand(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(result.ok);
    assert.ok(result.replyText.includes("proposal_abc"));
    assert.ok(result.replyText.includes("snap_new"));
    assert.strictEqual(callCount, 2);
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
        json: () => Promise.resolve({ status: "Rejected" }),
      });
    });
    const result = await executeApprovalCommand(config, { kind: "reject", proposalId: "proposal_abc", reason: "need more testing" });
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
    const result = await executeApprovalCommand(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("digest_mismatch") || result.replyText.includes("失败"));
  });

  it("fails when proposal not PendingApproval", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve({ ...mockProposalJson(), status: "Activated" }) }),
    );
    const result = await executeApprovalCommand(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("Activated"));
  });

  it("fails with network error", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: false, status: 500, json: () => Promise.resolve({ error: "internal_error" }) }),
    );
    const result = await executeApprovalCommand(config, { kind: "approve", proposalId: "proposal_abc", reason: "" });
    assert.ok(!result.ok);
  });

  it("fails with proposal_not_found", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ error: "proposal_not_found" }) }),
    );
    const result = await executeApprovalCommand(config, { kind: "approve", proposalId: "proposal_nonexistent", reason: "" });
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("不存在") || result.replyText.includes("not_found"));
  });
});
