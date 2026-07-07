import { describe, it } from "node:test";
import assert from "node:assert";
import {
  renderProposalPending,
  renderDecisionApproved,
  renderDecisionRejected,
  renderToolCall,
  renderReceiptSucceeded,
  renderReceiptFailed,
  splitLongText,
  renderError,
} from "./renderer.js";

describe("renderProposalPending", () => {
  it("includes all key fields", () => {
    const out = renderProposalPending({
      proposal_id: "proposal_abc",
      operation_name: "external.calculator",
      manifest_id: "manifest_xyz",
      artifact_digest: "sha256:abc123",
      endpoint: "http://127.0.0.1:7300/execute",
      risk: "External",
    });
    assert.ok(out.includes("proposal_abc"));
    assert.ok(out.includes("批准"));
    assert.ok(out.includes("拒绝"));
  });
  it("omitted optional fields are safe", () => {
    const out = renderProposalPending({ proposal_id: "p1", operation_name: "op", manifest_id: "m1" });
    assert.ok(!out.includes("undefined"));
  });
});

describe("renderDecisionApproved", () => {
  it("includes proposal and snapshot", () => {
    const out = renderDecisionApproved({ proposal_id: "p1", activated_snapshot_id: "s1" });
    assert.ok(out.includes("✅"));
    assert.ok(out.includes("p1"));
  });
});

describe("renderDecisionRejected", () => {
  it("includes proposal and reason", () => {
    const out = renderDecisionRejected({ proposal_id: "p1", reason: "need more testing" });
    assert.ok(out.includes("❌"));
    assert.ok(out.includes("need more testing"));
  });
});

describe("renderToolCall", () => {
  it("includes operation name", () => {
    const out = renderToolCall("external.calc", { a: 1 });
    assert.ok(out.includes("external.calc"));
  });
});

describe("renderReceiptSucceeded", () => {
  it("renders numeric output", () => {
    assert.ok(renderReceiptSucceeded(42).includes("42"));
  });
});

describe("renderReceiptFailed", () => {
  it("renders domain error", () => {
    const out = renderReceiptFailed({ error_category: "artifact_domain_error", harness_error_code: "divide_by_zero" });
    assert.ok(out.includes("divide_by_zero"));
  });
  it("handles empty input", () => {
    assert.ok(renderReceiptFailed({}).includes("执行失败"));
  });
});

describe("splitLongText", () => {
  it("short text returns single part", () => {
    assert.strictEqual(splitLongText("hello", 100).length, 1);
  });
  it("splits long text", () => {
    const parts = splitLongText("x".repeat(3000), 1500);
    assert.ok(parts.length >= 2);
    assert.ok(parts.every((p) => p.length <= 1500));
  });
});

describe("renderError", () => {
  it("redacts Bearer tokens", () => {
    const safe = renderError("unauthorized: Bearer sk-abc123");
    assert.ok(!safe.includes("sk-abc123"));
    assert.ok(safe.includes("[REDACTED]"));
  });
  it("redacts token_ patterns", () => {
    const safe = renderError("invalid token_abc123def456");
    assert.ok(!safe.includes("token_abc123def456"));
  });
  it("short messages pass through", () => {
    assert.strictEqual(renderError("not found"), "not found");
  });
});
