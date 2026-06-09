import { runAgentTurn } from "../../agent/src/index.mjs";
import { appendEvent, appendStateRecord, createEvent, errorEnvelope, okEnvelope } from "../../core/src/index.mjs";
import { createToolRegistry } from "../../tools/src/index.mjs";
import { buildFeishuConfig } from "./config.mjs";
import { createFeishuRestClient } from "./client.mjs";
import { normalizeFeishuMessageEvent } from "./normalize.mjs";
import { evaluateFeishuIngressPolicy } from "./policy.mjs";
import { completeFeishuMessage, reserveFeishuMessage } from "./replay.mjs";

export async function handleFeishuAgentEvent(raw, options = {}) {
  const started = Date.now();
  const config = buildFeishuConfig(options.config || options);
  const stateDir = options.stateDir;
  const client = options.client || createFeishuRestClient({ config, fetchImpl: options.fetchImpl });
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

  const agent = await runAgentTurn({
    text: inbound.content.text,
    provider: options.provider,
    stateDir,
    source: "feishu",
    sessionId: sessionIdFor(inbound),
    systemPrompt: options.systemPrompt,
    registry: options.registry || createToolRegistry([]),
    maxIterations: options.maxIterations || 3,
  });
  await appendEvent(stateDir, createEvent("channel.message.received", {
    runId: agent.runId || null,
    channel: "feishu",
    messageId: inbound.messageId,
    chatId: inbound.chatId,
  }));

  const replyText = limitFeishuReply(friendlyAgentReply(agent), config.maxReplyChars);
  try {
    const receipt = await client.replyText({ messageId: inbound.messageId, chatId: inbound.chatId, text: replyText });
    await completeFeishuMessage(stateDir, inbound.messageId, {
      runId: agent.runId || null,
      replyMessageId: receipt.messageId || null,
    });
    await recordFeishuReply(stateDir, { runId: agent.runId, inbound, replyText, receipt, status: "sent" });
    await appendEvent(stateDir, createEvent("reply.sent", {
      runId: agent.runId || null,
      channel: "feishu",
      messageId: receipt.messageId || null,
    }));
    return okEnvelope({
      runId: agent.runId,
      result: { type: "feishu-agent-reply", inbound, agent, reply: { text: replyText, receipt } },
      usage: { elapsedMs: Date.now() - started },
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    await completeFeishuMessage(stateDir, inbound.messageId, {
      runId: agent.runId || null,
      status: "reply_failed",
      error: message,
    });
    await recordFeishuReply(stateDir, { runId: agent.runId, inbound, replyText, status: "failed", error: message });
    await appendEvent(stateDir, createEvent("reply.failed", {
      runId: agent.runId || null,
      channel: "feishu",
      messageId: inbound.messageId,
    }));
    return errorEnvelope({ runId: agent.runId, code: "feishu_reply_failed", message, usage: { elapsedMs: Date.now() - started } });
  }
}

export function friendlyAgentReply(agent) {
  if (agent.ok && agent.status === "needs_approval") {
    return "这个飞书入口目前只支持聊天，不会执行需要审批的工具操作。";
  }
  if (agent.ok) {
    return agent.result?.answer || "我处理完了，但没有生成可发送的文本。";
  }
  if (agent.error?.code === "model_config_required") {
    return "我这边模型还没有配置好，暂时不能生成回复。";
  }
  if (agent.error?.code === "tool_not_found") {
    return "这个飞书入口目前只支持聊天，不会执行工具操作。";
  }
  return "我这边处理失败了，已经记录下来，稍后可以从本地日志继续排查。";
}

export function limitFeishuReply(text, maxChars = 1800) {
  const normalized = String(text || "").trim() || "我处理完了。";
  if (normalized.length <= maxChars) {
    return normalized;
  }
  const suffix = "\n\n[回复过长，已截断]";
  return `${normalized.slice(0, Math.max(0, maxChars - suffix.length))}${suffix}`;
}

async function recordFeishuReply(stateDir, input) {
  await appendStateRecord(stateDir, "feishu_replies.jsonl", {
    runId: input.runId || null,
    inboundMessageId: input.inbound.messageId,
    chatId: input.inbound.chatId,
    status: input.status,
    replyMessageId: input.receipt?.messageId || null,
    textSummary: summarize(input.replyText),
    error: input.error || null,
    at: new Date().toISOString(),
  });
}

function sessionIdFor(inbound) {
  return inbound.chatType === "p2p" ? `feishu:p2p:${inbound.sender.openId}` : `feishu:group:${inbound.chatId}`;
}

function summarize(value) {
  const text = String(value || "").replaceAll(/\s+/g, " ").trim();
  return text.length > 160 ? `${text.slice(0, 157)}...` : text;
}
