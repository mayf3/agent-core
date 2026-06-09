import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { readEvents, readStateRecords } from "../../core/src/index.mjs";
import {
  createFeishuRestClient,
  createMemoryFeishuClient,
  handleFeishuAgentEvent,
  handleFeishuEchoEvent,
} from "../src/index.mjs";

test("feishu echo replies to allowed private text", async () => {
  const env = await tempEnv();
  try {
    const client = createMemoryFeishuClient();
    const result = await handleFeishuEchoEvent(sampleEvent(), {
      stateDir: env.stateDir,
      client,
      config: { allowedOpenIds: ["ou_user"] },
    });

    assert.equal(result.ok, true);
    assert.equal(result.result.reply.text, "收到：你好");
    assert.equal(client.replies.length, 1);
    assert.equal((await readEvents(env.stateDir, { runId: result.runId })).some((event) => event.type === "reply.sent"), true);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("feishu echo deduplicates message ids", async () => {
  const env = await tempEnv();
  try {
    const client = createMemoryFeishuClient();
    const options = { stateDir: env.stateDir, client, config: { allowedOpenIds: ["ou_user"] } };
    await handleFeishuEchoEvent(sampleEvent(), options);
    const duplicate = await handleFeishuEchoEvent(sampleEvent(), options);

    assert.equal(duplicate.result.duplicate, true);
    assert.equal(client.replies.length, 1);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("feishu policy blocks unmentioned group messages", async () => {
  const env = await tempEnv();
  try {
    const client = createMemoryFeishuClient();
    const result = await handleFeishuEchoEvent(sampleEvent({
      message: { chat_type: "group", chat_id: "oc_chat", mentions: [] },
    }), {
      stateDir: env.stateDir,
      client,
      config: { allowedChatIds: ["oc_chat"], requireGroupMention: true, botOpenId: "ou_bot" },
    });

    assert.equal(result.status, "skipped");
    assert.equal(result.result.reason, "bot_not_mentioned");
    assert.equal(client.replies.length, 0);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("feishu policy accepts mentioned group messages", async () => {
  const env = await tempEnv();
  try {
    const client = createMemoryFeishuClient();
    const result = await handleFeishuEchoEvent(sampleEvent({
      message: {
        chat_type: "group",
        chat_id: "oc_chat",
        mentions: [{ key: "@_user_1", id: { open_id: "ou_bot" } }],
      },
    }), {
      stateDir: env.stateDir,
      client,
      config: { allowedChatIds: ["oc_chat"], requireGroupMention: true, botOpenId: "ou_bot" },
    });

    assert.equal(result.ok, true);
    assert.equal(client.replies.length, 1);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("feishu agent replies through a chat-only model turn", async () => {
  const env = await tempEnv();
  try {
    const client = createMemoryFeishuClient();
    const provider = fakeProvider((input) => {
      assert.equal(input.tools.length, 0);
      return { ok: true, text: "你好，我在。", toolCalls: [] };
    });
    const result = await handleFeishuAgentEvent(sampleEvent(), {
      stateDir: env.stateDir,
      client,
      provider,
      config: { allowedOpenIds: ["ou_user"] },
    });
    const replies = await readStateRecords(env.stateDir, "feishu_replies.jsonl");
    const snapshots = await readStateRecords(env.stateDir, "context_snapshots.jsonl");

    assert.equal(result.ok, true);
    assert.equal(result.result.reply.text, "你好，我在。");
    assert.equal(client.replies.length, 1);
    assert.equal(replies[0].status, "sent");
    assert.deepEqual(snapshots[0].toolNames, []);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("feishu agent sends friendly model config errors", async () => {
  const env = await tempEnv();
  try {
    const client = createMemoryFeishuClient();
    const provider = fakeProvider(() => ({
      ok: false,
      status: "needs_config",
      error: { code: "model_config_required", message: "missing model" },
    }));
    const result = await handleFeishuAgentEvent(sampleEvent(), {
      stateDir: env.stateDir,
      client,
      provider,
      config: { allowedOpenIds: ["ou_user"] },
    });

    assert.equal(result.ok, true);
    assert.match(result.result.reply.text, /模型还没有配置好/);
    assert.equal(client.replies.length, 1);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("feishu agent limits long replies", async () => {
  const env = await tempEnv();
  try {
    const client = createMemoryFeishuClient();
    const provider = fakeProvider(() => ({ ok: true, text: "很".repeat(80), toolCalls: [] }));
    const result = await handleFeishuAgentEvent(sampleEvent(), {
      stateDir: env.stateDir,
      client,
      provider,
      config: { allowedOpenIds: ["ou_user"], maxReplyChars: 32 },
    });

    assert.equal(result.result.reply.text.length, 32);
    assert.match(result.result.reply.text, /已截断/);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("feishu rest client obtains a token and replies to a message", async () => {
  const requests = [];
  const client = createFeishuRestClient({
    appId: "app-test",
    appSecret: "redacted",
    fetchImpl: async (url, init) => {
      requests.push({ url, init });
      if (url.endsWith("/tenant_access_token/internal")) {
        return jsonResponse({ code: 0, tenant_access_token: "tok", expire: 3600 });
      }
      return jsonResponse({ code: 0, data: { message_id: "om_reply" } });
    },
  });
  const receipt = await client.replyText({ messageId: "om_msg", chatId: "oc_chat", text: "hi" });

  assert.equal(receipt.messageId, "om_reply");
  assert.equal(requests.length, 2);
  assert.equal(JSON.parse(requests[1].init.body).msg_type, "text");
});

function sampleEvent(patch = {}) {
  return {
    header: { event_id: "evt_1" },
    event: {
      sender: { sender_type: "user", sender_id: { open_id: "ou_user" } },
      message: {
        message_id: "om_msg",
        chat_id: "oc_chat",
        chat_type: "p2p",
        message_type: "text",
        content: "{\"text\":\"你好\"}",
        mentions: [],
        ...patch.message,
      },
      ...patch.event,
    },
    ...patch.raw,
  };
}

async function tempEnv() {
  const root = await mkdtemp(path.join(os.tmpdir(), "agent-core-feishu-"));
  return { root, stateDir: path.join(root, "state") };
}

function fakeProvider(handler) {
  return {
    name: "fake",
    model: "fake-model",
    async generate(input) {
      return handler(input);
    },
  };
}

function jsonResponse(body, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    async json() {
      return body;
    },
  };
}
