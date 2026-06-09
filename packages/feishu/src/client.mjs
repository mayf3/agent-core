import { buildFeishuConfig } from "./config.mjs";

const defaultBaseUrl = "https://open.feishu.cn";

export function createFeishuRestClient(options = {}) {
  const config = buildFeishuConfig(options.config || options);
  const fetchImpl = options.fetchImpl || fetch;
  const baseUrl = String(options.baseUrl || defaultBaseUrl).replace(/\/$/, "");
  let tokenCache = null;
  return {
    async replyText(input) {
      const token = await tenantAccessToken({ config, fetchImpl, baseUrl, tokenCache });
      tokenCache = token;
      const response = await fetchImpl(`${baseUrl}/open-apis/im/v1/messages/${encodeURIComponent(input.messageId)}/reply`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${token.value}`,
          "content-type": "application/json",
        },
        body: JSON.stringify({
          msg_type: "text",
          content: JSON.stringify({ text: input.text }),
        }),
      });
      const body = await readJson(response);
      assertFeishuOk(response, body, "feishu_reply_failed");
      return {
        provider: "feishu-rest",
        messageId: body?.data?.message_id || body?.data?.message?.message_id || null,
        chatId: input.chatId || null,
        sentAt: new Date().toISOString(),
      };
    },
  };
}

async function tenantAccessToken({ config, fetchImpl, baseUrl, tokenCache }) {
  if (tokenCache && tokenCache.expiresAt > Date.now() + 60000) {
    return tokenCache;
  }
  if (!config.appId || !config.appSecret) {
    throw new Error("Feishu app id and app secret are required.");
  }
  const response = await fetchImpl(`${baseUrl}/open-apis/auth/v3/tenant_access_token/internal`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      app_id: config.appId,
      app_secret: secretValue(config),
    }),
  });
  const body = await readJson(response);
  assertFeishuOk(response, body, "feishu_token_failed");
  const value = body.tenant_access_token;
  if (!value) {
    throw new Error("Feishu tenant access token response was empty.");
  }
  const expiresIn = Number(body.expire || 3600);
  return { value, expiresAt: Date.now() + expiresIn * 1000 };
}

function secretValue(config) {
  return config.appSecret;
}

async function readJson(response) {
  try {
    return await response.json();
  } catch {
    return {};
  }
}

function assertFeishuOk(response, body, fallbackCode) {
  if (!response.ok || Number(body?.code || 0) !== 0) {
    const code = body?.code ? `${fallbackCode}_${body.code}` : fallbackCode;
    const message = body?.msg || body?.message || `Feishu request failed with HTTP ${response.status}.`;
    throw new Error(`${code}: ${message}`);
  }
}
