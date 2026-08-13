import test from "node:test";
import assert from "node:assert/strict";
import { parseChatControl, renderChatControl, tryHandleChatControl } from "./control.js";
import type { FeishuTransport } from "./transport.js";

test("only the three rescue commands are intercepted", () => {
  assert.equal(parseChatControl(" /status "), "status");
  assert.equal(parseChatControl("/stop"), "stop");
  assert.equal(parseChatControl("/reset"), "reset");
  assert.equal(parseChatControl("继续"), null);
  assert.equal(parseChatControl("修复 connector"), null);
});

test("status rendering uses only host facts", () => {
  const text = renderChatControl("status", {
    session_id: "session_1",
    run: {
      run_id: "run_1",
      status: "Failed",
      task: "修复失败反馈",
      elapsed_ms: 200_000,
      tool_calls_used: 64,
      tool_calls_max: 64,
      recent_activity: { kind: "RunFailed", detail: "runtime_v0_failed" },
      code_modified: true,
      failure_reason: "tool_call_limit_reached: exec",
    },
  });
  assert.match(text, /状态：Failed/);
  assert.match(text, /工具调用：64 \/ 64/);
  assert.match(text, /原因：tool_call_limit_reached/);
});

test("control request bypasses ingress and replies immediately", async () => {
  const replies: string[] = [];
  const transport: FeishuTransport = {
    async replyToMessage(_messageId, _type, content: any) {
      replies.push(content.text);
      return { message_id: "reply_1", status: "sent" };
    },
  };
  let requestedUrl = "";
  let requestBody: any;
  const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
    requestedUrl = String(url);
    requestBody = JSON.parse(String(init?.body));
    return new Response(
      JSON.stringify({ ok: true, action: "stop", run_id: "run_1", state: "cancellation_requested" }),
      { status: 200, headers: { "content-type": "application/json" } },
    );
  }) as typeof fetch;
  const handled = await tryHandleChatControl(
    {
      kernelUrl: "http://127.0.0.1:4130/v1/ingress",
      kernelIngressTimeoutMs: 100,
      ipcToken: "ipc",
    } as any,
    {
      message_type: "text",
      text: "/stop",
      message_id: "om_1",
      chat_type: "p2p",
      sender_open_id: "ou_1",
      chat_id: "oc_1",
    },
    transport,
    fetcher,
  );
  assert.equal(handled, true);
  assert.equal(requestedUrl, "http://127.0.0.1:4130/v1/chat-control");
  assert.equal(requestBody.action, "stop");
  assert.equal(requestBody.conversation_key, "feishu:open_id:ou_1");
  assert.match(replies[0], /已请求停止/);
});
