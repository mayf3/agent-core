import {
  appendEvent,
  createEvent,
  createRunRecord,
  okEnvelope,
  recordRun,
  updateRunStatus,
} from "../../core/src/index.mjs";
import { buildFeishuConfig } from "./config.mjs";
import { normalizeFeishuMessageEvent } from "./normalize.mjs";
import { evaluateFeishuIngressPolicy } from "./policy.mjs";
import { completeFeishuMessage, reserveFeishuMessage } from "./replay.mjs";

export async function handleFeishuEchoEvent(raw, options = {}) {
  const started = Date.now();
  const config = buildFeishuConfig(options.config || options);
  const stateDir = options.stateDir;
  const client = options.client || createMemoryFeishuClient();
  const inbound = normalizeFeishuMessageEvent(raw);
  const policy = evaluateFeishuIngressPolicy(inbound, config);
  if (!policy.ok) {
    const event = await appendEvent(stateDir, createEvent("channel.message.skipped", {
      channel: "feishu",
      messageId: inbound.messageId || null,
      reason: policy.reason,
    }));
    return okEnvelope({ status: "skipped", result: { skipped: true, reason: policy.reason }, events: [event] });
  }

  const reservation = await reserveFeishuMessage(stateDir, inbound);
  if (reservation.duplicate) {
    const event = await appendEvent(stateDir, createEvent("channel.message.duplicate", {
      channel: "feishu",
      messageId: inbound.messageId,
    }));
    return okEnvelope({ status: "skipped", result: { duplicate: true }, events: [event] });
  }

  const run = createRunRecord({
    prefix: "feishu",
    source: "feishu",
    sessionId: sessionIdFor(inbound),
    status: "running",
    inputSummary: summarize(inbound.content.text),
  });
  await recordRun(stateDir, run);
  await appendEvent(stateDir, createEvent("run.started", { runId: run.runId, source: "feishu" }));
  await appendEvent(stateDir, createEvent("channel.message.received", {
    runId: run.runId,
    channel: "feishu",
    messageId: inbound.messageId,
    chatId: inbound.chatId,
  }));

  const replyText = options.replyBuilder ? await options.replyBuilder(inbound) : `收到：${inbound.content.text}`;
  const receipt = await client.replyText({ messageId: inbound.messageId, chatId: inbound.chatId, text: replyText });
  await completeFeishuMessage(stateDir, inbound.messageId, {
    runId: run.runId,
    replyMessageId: receipt.messageId || null,
  });
  await appendEvent(stateDir, createEvent("reply.sent", {
    runId: run.runId,
    channel: "feishu",
    messageId: receipt.messageId || null,
  }));
  await updateRunStatus(stateDir, run.runId, "ok", { resultSummary: "feishu echo replied" });
  return okEnvelope({
    runId: run.runId,
    result: { type: "feishu-echo", inbound, reply: { text: replyText, receipt } },
    usage: { elapsedMs: Date.now() - started },
  });
}

export function createMemoryFeishuClient() {
  const replies = [];
  return {
    replies,
    async replyText(input) {
      const receipt = {
        messageId: `reply_${input.messageId}`,
        chatId: input.chatId,
        text: input.text,
        sentAt: new Date().toISOString(),
      };
      replies.push(receipt);
      return receipt;
    },
  };
}

function sessionIdFor(inbound) {
  return inbound.chatType === "p2p" ? `feishu:p2p:${inbound.sender.openId}` : `feishu:group:${inbound.chatId}`;
}

function summarize(value) {
  const text = String(value || "").replaceAll(/\s+/g, " ").trim();
  return text.length > 160 ? `${text.slice(0, 157)}...` : text;
}
