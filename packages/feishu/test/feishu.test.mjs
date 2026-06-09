import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { readEvents } from "../../core/src/index.mjs";
import { createMemoryFeishuClient, handleFeishuEchoEvent } from "../src/index.mjs";

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
