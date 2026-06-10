import { randomUUID } from "node:crypto";
import type { ConnectorConfig } from "./config.js";

export async function postIngress(config: ConnectorConfig, event: unknown) {
  const normalized = normalizeMessageEvent(event);
  console.log(`feishu event received type=${normalized.payload.message_type} chat=${normalized.payload.chat_type} msg=${shortId(normalized.payload.message_id)}`);
  const body = {
    protocol_version: "v1",
    source: "Feishu",
    external_event_id: normalized.external_event_id,
    received_at: new Date().toISOString(),
    payload: normalized.payload,
    routing_hint: {},
  };
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 2500);
  try {
    const response = await fetch(config.kernelUrl, {
      method: "POST",
      signal: controller.signal,
      headers: {
        authorization: `Bearer ${config.ipcToken}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      console.error(`kernel ingress failed: HTTP ${response.status}`);
      return;
    }
    const result = await response.json().catch(() => ({}));
    console.log(`kernel ingress result status=${result.status || "unknown"} run=${shortId(result.run_id || "")}`);
  } finally {
    clearTimeout(timer);
  }
}

function normalizeMessageEvent(raw: any) {
  const event = raw?.event || raw;
  const header = raw?.header || event?.header || {};
  const message = event?.message || {};
  const sender = event?.sender || {};
  const content = parseContent(message.content);
  const messageId = message.message_id || raw?.message_id || "";
  return {
    external_event_id: header.event_id || raw?.event_id || messageId || `feishu_${randomUUID()}`,
    payload: {
      sender_open_id: sender.sender_id?.open_id || raw?.open_id || "",
      sender_type: sender.sender_type || raw?.sender_type || "user",
      chat_id: message.chat_id || raw?.chat_id || "",
      chat_type: message.chat_type || raw?.chat_type || "p2p",
      message_id: messageId,
      message_type: message.message_type || content.type || "text",
      text: content.text || "",
      mentions: normalizeMentions(message.mentions || content.mentions || []),
    },
  };
}

function parseContent(value: unknown): any {
  if (!value) {
    return {};
  }
  if (typeof value === "object") {
    return value;
  }
  try {
    return JSON.parse(String(value));
  } catch {
    return { text: String(value) };
  }
}

function normalizeMentions(values: any[]) {
  return values.map((mention) => ({
    open_id: mention.id?.open_id || mention.open_id || "",
    name: mention.name || mention.id?.name || "",
  }));
}

function shortId(value: string) {
  if (!value) {
    return "-";
  }
  return value.length <= 10 ? value : `${value.slice(0, 4)}...${value.slice(-4)}`;
}
