import { describe, it, mock } from "node:test";
import assert from "node:assert";
import {
  parseHarnessChangeCommand,
  isHarnessChangeAuthorised,
  isUnsupportedCommand,
  postHarnessChangeRequest,
  type HCRConfig,
} from "./harness-command.js";

const ownerOpenId = "ou_owner123";
const makeConfig = (overrides?: Partial<HCRConfig>): HCRConfig => ({
  kernelBaseUrl: "http://127.0.0.1:4130",
  ipcToken: "test-ipc-token",
  ownerOpenId,
  ...overrides,
});

// ═══════════════════════════════════════════════════════════════════
// Command parser tests
// ═══════════════════════════════════════════════════════════════════

describe("parseHarnessChangeCommand", () => {
  it("valid Chinese command with Chinese colon", () => {
    const cmd = parseHarnessChangeCommand("创建 Harness my-test-helper：帮我写一个代码审查助手");
    assert.deepStrictEqual(cmd, {
      harnessId: "my-test-helper",
      requirement: "帮我写一个代码审查助手",
    });
  });

  it("valid Chinese command with ASCII colon", () => {
    const cmd = parseHarnessChangeCommand("创建 Harness my-test-helper:帮我写一个代码审查助手");
    assert.deepStrictEqual(cmd, {
      harnessId: "my-test-helper",
      requirement: "帮我写一个代码审查助手",
    });
  });

  it("uppercase harness_id rejected", () => {
    assert.strictEqual(parseHarnessChangeCommand("创建 Harness FOO：test"), null);
  });

  it("underscore harness_id rejected", () => {
    assert.strictEqual(parseHarnessChangeCommand("创建 Harness foo_bar：test"), null);
  });

  it("leading hyphen harness_id rejected", () => {
    assert.strictEqual(parseHarnessChangeCommand("创建 Harness -leading：test"), null);
  });

  it("trailing hyphen harness_id rejected", () => {
    assert.strictEqual(parseHarnessChangeCommand("创建 Harness trailing-：test"), null);
  });

  it("double hyphen harness_id rejected", () => {
    assert.strictEqual(parseHarnessChangeCommand("创建 Harness foo--bar：test"), null);
  });

  it("trim whitespace", () => {
    const cmd = parseHarnessChangeCommand("  创建 Harness my-test-helper：帮我写一个代码审查助手  ");
    assert.deepStrictEqual(cmd, {
      harnessId: "my-test-helper",
      requirement: "帮我写一个代码审查助手",
    });
  });

  it("non-HCR text returns null", () => {
    assert.strictEqual(parseHarnessChangeCommand("6 * 7 等于多少？"), null);
  });

  it("empty string returns null", () => {
    assert.strictEqual(parseHarnessChangeCommand(""), null);
  });

  it("'创建 Harness' without id returns null", () => {
    assert.strictEqual(parseHarnessChangeCommand("创建 Harness"), null);
  });
});

// ═══════════════════════════════════════════════════════════════════
// Authorisation tests
// ═══════════════════════════════════════════════════════════════════

describe("isHarnessChangeAuthorised", () => {
  it("owner + p2p → allowed", () => {
    assert.strictEqual(isHarnessChangeAuthorised(makeConfig(), "p2p", ownerOpenId), null);
  });

  it("non-owner + p2p → rejected", () => {
    const err = isHarnessChangeAuthorised(makeConfig(), "p2p", "ou_stranger");
    assert.ok(err?.includes("创建人"));
  });

  it("owner + group → rejected", () => {
    const err = isHarnessChangeAuthorised(makeConfig(), "group", ownerOpenId);
    assert.ok(err?.includes("仅支持私聊"));
  });

  it("missing owner → rejected", () => {
    const err = isHarnessChangeAuthorised(makeConfig({ ownerOpenId: undefined }), "p2p", ownerOpenId);
    assert.ok(err?.includes("未配置"));
  });
});

// ═══════════════════════════════════════════════════════════════════
// Unsupported command detection tests
// ═══════════════════════════════════════════════════════════════════

describe("isUnsupportedCommand", () => {
  it("修改 Harness detected", () => {
    assert.ok(isUnsupportedCommand("修改 Harness my-tool：update"));
  });
  it("优化 Harness detected", () => {
    assert.ok(isUnsupportedCommand("优化 Harness my-tool：optimize"));
  });
  it("删除 Harness detected", () => {
    assert.ok(isUnsupportedCommand("删除 Harness my-tool：delete"));
  });
  it("注册 Harness detected", () => {
    assert.ok(isUnsupportedCommand("注册 Harness my-tool：register"));
  });
  it("启用 Harness detected", () => {
    assert.ok(isUnsupportedCommand("启用 Harness my-tool：enable"));
  });
  it("创建 Harness NOT detected (it is the supported command)", () => {
    assert.ok(!isUnsupportedCommand("创建 Harness my-tool：create"));
  });
  it("ordinary text NOT detected", () => {
    assert.ok(!isUnsupportedCommand("批准 proposal_abc"));
  });
  it("empty string NOT detected", () => {
    assert.ok(!isUnsupportedCommand(""));
  });
});

// ═══════════════════════════════════════════════════════════════════
// postHarnessChangeRequest tests
// ═══════════════════════════════════════════════════════════════════

describe("postHarnessChangeRequest", () => {
  it("success response returns request_id and pending status", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve({
          ok: true,
          request_id: "hcr_abc123",
          status: "pending",
          deduplicated: false,
        }),
      }),
    );
    const result = await postHarnessChangeRequest(config, "my-harness", "test requirement", "om_msg_001", {});
    assert.ok(result.ok);
    assert.ok(result.replyText.includes("hcr_abc123"));
    assert.ok(result.replyText.includes("等待开发执行"));
    assert.strictEqual(result.requestId, "hcr_abc123");
    assert.strictEqual(result.deduplicated, false);
  });

  it("duplicate delivery returns deduplicated=true", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve({
          ok: true,
          request_id: "hcr_abc123",
          status: "pending",
          deduplicated: true,
        }),
      }),
    );
    const result = await postHarnessChangeRequest(config, "my-harness", "test requirement", "om_msg_001", {});
    assert.ok(result.ok);
    assert.strictEqual(result.deduplicated, true);
    assert.ok(result.replyText.includes("重复消息"));
  });

  it("owner_required error", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({
        ok: false, status: 403,
        json: () => Promise.resolve({ error: "HARNESS_CHANGE_REQUEST_OWNER_REQUIRED" }),
      }),
    );
    const result = await postHarnessChangeRequest(config, "my-harness", "test", "om_msg_002", {});
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("创建人"));
  });

  it("p2p_required error", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({
        ok: false, status: 403,
        json: () => Promise.resolve({ error: "HARNESS_CHANGE_REQUEST_P2P_REQUIRED" }),
      }),
    );
    const result = await postHarnessChangeRequest(config, "my-harness", "test", "om_msg_003", {});
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("仅支持私聊"));
  });

  it("invalid_harness_id error", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({
        ok: false, status: 400,
        json: () => Promise.resolve({ error: "INVALID_HARNESS_ID" }),
      }),
    );
    const result = await postHarnessChangeRequest(config, "FOO", "test", "om_msg_004", {});
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("小写字母"));
  });

  it("empty_requirement error", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({
        ok: false, status: 400,
        json: () => Promise.resolve({ error: "EMPTY_HARNESS_REQUIREMENT" }),
      }),
    );
    const result = await postHarnessChangeRequest(config, "my-harness", "", "om_msg_005", {});
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("不能为空"));
  });

  it("source_message_id is forwarded in body", async () => {
    const config = makeConfig();
    let body: any = null;
    (globalThis as any).fetch = mock.fn((url: string, options: any) => {
      body = JSON.parse(options.body);
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () => Promise.resolve({
          ok: true,
          request_id: "hcr_abc",
          status: "pending",
          deduplicated: false,
        }),
      });
    });
    await postHarnessChangeRequest(config, "my-harness", "test", "om_original_msg_id", { event: "data" });
    assert.strictEqual(body.source_message_id, "om_original_msg_id");
    assert.strictEqual(body.harness_id, "my-harness");
    assert.strictEqual(body.requirement, "test");
    assert.ok(body.payload); // original payload is forwarded
  });

  it("network error returns stable message", async () => {
    const config = makeConfig();
    (globalThis as any).fetch = mock.fn(() =>
      Promise.resolve({
        ok: false, status: 500,
        json: () => Promise.resolve({ error: "HARNESS_CHANGE_REQUEST_INTERNAL_ERROR" }),
      }),
    );
    const result = await postHarnessChangeRequest(config, "my-harness", "test", "om_msg_006", {});
    assert.ok(!result.ok);
    assert.ok(result.replyText.includes("请稍后重试"));
  });
});
