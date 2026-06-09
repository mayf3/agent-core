export function normalizeFeishuMessageEvent(raw) {
  const event = raw?.event || raw;
  const message = event?.message || {};
  const sender = event?.sender || {};
  const content = parseContent(message.content);
  return {
    channel: "feishu",
    externalEventId: raw?.header?.event_id || raw?.event_id || null,
    messageId: message.message_id || raw?.message_id || "",
    chatId: message.chat_id || raw?.chat_id || "",
    chatType: normalizeChatType(message.chat_type || raw?.chat_type),
    threadId: message.thread_id || message.parent_id || null,
    sender: {
      senderType: sender.sender_type || raw?.sender_type || "user",
      openId: sender.sender_id?.open_id || raw?.open_id || "",
      userId: sender.sender_id?.user_id || raw?.user_id || "",
      unionId: sender.sender_id?.union_id || raw?.union_id || "",
    },
    content: {
      type: message.message_type || content.type || "text",
      text: content.text || "",
    },
    mentions: normalizeMentions(message.mentions || content.mentions || []),
    createdAt: Number(message.create_time || raw?.created_at || Date.now()),
    raw,
  };
}

function parseContent(value) {
  if (!value) {
    return {};
  }
  if (typeof value === "object") {
    return value;
  }
  try {
    return JSON.parse(value);
  } catch {
    return { text: String(value) };
  }
}

function normalizeChatType(value) {
  return value === "group" ? "group" : "p2p";
}

function normalizeMentions(values) {
  return values.map((mention) => ({
    key: mention.key || "",
    name: mention.name || mention.id?.name || "",
    openId: mention.id?.open_id || mention.open_id || "",
  }));
}
