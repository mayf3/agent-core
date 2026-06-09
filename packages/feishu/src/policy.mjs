export function evaluateFeishuIngressPolicy(inbound, config) {
  if (!inbound.messageId) {
    return deny("missing_message_id");
  }
  if (inbound.sender.senderType === "app") {
    return deny("bot_sender");
  }
  if (inbound.content.type !== "text") {
    return deny("unsupported_message_type");
  }
  if (!inbound.content.text.trim()) {
    return deny("empty_text");
  }
  if (inbound.chatType === "p2p") {
    if (config.allowedOpenIds.length && !config.allowedOpenIds.includes(inbound.sender.openId)) {
      return deny("sender_not_allowed");
    }
    return allow();
  }
  if (config.allowedChatIds.length && !config.allowedChatIds.includes(inbound.chatId)) {
    return deny("chat_not_allowed");
  }
  if (config.requireGroupMention && !hasBotMention(inbound, config.botOpenId)) {
    return deny("bot_not_mentioned");
  }
  return allow();
}

function hasBotMention(inbound, botOpenId) {
  if (!botOpenId) {
    return inbound.mentions.length > 0;
  }
  return inbound.mentions.some((mention) => mention.openId === botOpenId);
}

function allow() {
  return { ok: true, reason: null };
}

function deny(reason) {
  return { ok: false, reason };
}
