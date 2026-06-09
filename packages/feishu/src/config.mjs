export function buildFeishuConfig(options = {}) {
  const env = options.env || process.env;
  return {
    appId: stringValue(options.appId || env.AGENT_CORE_FEISHU_APP_ID),
    appSecret: stringValue(options.appSecret || env.AGENT_CORE_FEISHU_APP_SECRET),
    appSecretConfigured: Boolean(options.appSecret || env.AGENT_CORE_FEISHU_APP_SECRET),
    allowedOpenIds: listValue(options.allowedOpenIds || env.AGENT_CORE_FEISHU_ALLOWED_OPEN_IDS),
    allowedChatIds: listValue(options.allowedChatIds || env.AGENT_CORE_FEISHU_ALLOWED_CHAT_IDS),
    requireGroupMention: booleanValue(options.requireGroupMention ?? env.AGENT_CORE_FEISHU_REQUIRE_GROUP_MENTION, true),
    botOpenId: stringValue(options.botOpenId || env.AGENT_CORE_FEISHU_BOT_OPEN_ID),
    maxReplyChars: numberValue(options.maxReplyChars || env.AGENT_CORE_FEISHU_MAX_REPLY_CHARS, 1800),
  };
}

export function describeFeishuReadiness(config) {
  return {
    appIdConfigured: Boolean(config.appId),
    appSecretConfigured: config.appSecretConfigured,
    hasAllowedOpenIds: config.allowedOpenIds.length > 0,
    hasAllowedChatIds: config.allowedChatIds.length > 0,
    requireGroupMention: config.requireGroupMention,
  };
}

function stringValue(value) {
  return String(value || "").trim();
}

function listValue(value) {
  if (Array.isArray(value)) {
    return value.map(stringValue).filter(Boolean);
  }
  return String(value || "").split(",").map(stringValue).filter(Boolean);
}

function booleanValue(value, fallback) {
  if (value === undefined || value === null || value === "") {
    return fallback;
  }
  return String(value).toLowerCase() === "true";
}

function numberValue(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}
