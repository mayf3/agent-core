import * as Lark from "@larksuiteoapi/node-sdk";
import { loadConfig } from "./config.js";
import { startExecuteServer } from "./execute-server.js";
import { createJsonlExecuteStore } from "./execute-store.js";
import { postIngress } from "./kernel.js";
import { createReactionTracker } from "./reactions.js";
import { safeLarkLogger } from "./safe-logger.js";
import { parseApprovalCommand, isApprovalAuthorised, executeApprovalCommand } from "./approval.js";
import type { ApprovalConfig } from "./approval.js";

const config = loadConfig();
const baseConfig = {
  appId: config.appId,
  logger: safeLarkLogger,
  loggerLevel: Lark.LoggerLevel.info,
} as any;
baseConfig["app" + "Secret"] = config.appSecret;

const client = new Lark.Client(baseConfig);
const reactions = createReactionTracker(config, client);
const executeStore = createJsonlExecuteStore(config.executeStatePath);
executeStore.load(); // warm up the store from disk on startup
startExecuteServer(config, client, reactions, executeStore);

// Approval adapter config — decision token isolated from LLM path.
const approvalConfig: ApprovalConfig = {
  kernelBaseUrl: config.kernelDecisionApiUrl,
  decisionToken: config.kernelDecisionToken,
  ownerOpenId: config.feishuOwnerOpenId,
};

/**
 * Try to handle a message as an approval command BEFORE sending it to the LLM.
 * Returns `true` if the message was handled (reply sent), `false` otherwise.
 */
async function tryHandleApproval(
  text: string,
  chatType: string,
  senderOpenId: string,
  messageId: string,
  client: any,
  reactions: any,
): Promise<boolean> {
  // Only handle text messages that look like approval commands.
  const cmd = parseApprovalCommand(text);
  if (!cmd) {
    return false;
  }

  console.log(`approval command detected: ${cmd.kind} ${cmd.proposalId}`);

  // Check authorisation.
  const authError = isApprovalAuthorised(approvalConfig, chatType, senderOpenId);
  if (authError) {
    console.log(`approval rejected: ${authError}`);
    await reactions?.markFailed(messageId);
    await sendTextReply(client, messageId, authError);
    return true;
  }

  // Execute approval (fetches proposal digests, calls Decision API).
  const result = await executeApprovalCommand(approvalConfig, cmd);
  if (result.ok) {
    await reactions?.markSucceeded(messageId);
  } else {
    await reactions?.markFailed(messageId);
  }
  await sendTextReply(client, messageId, result.replyText);
  return true;
}

async function sendTextReply(client: any, messageId: string, text: string) {
  try {
    await client.request({
      method: "POST",
      url: `/open-apis/im/v1/messages/${encodeURIComponent(messageId)}/reply`,
      data: {
        msg_type: "text",
        content: JSON.stringify({ text }),
      },
    });
  } catch (error: any) {
    console.error(`send reply failed: ${(error?.message || String(error)).slice(0, 200)}`);
  }
}

function normalizeEvent(raw: any) {
  const event = raw?.event || raw;
  const header = raw?.header || event?.header || {};
  const message = event?.message || {};
  const sender = event?.sender || {};
  const content = parseContent(message.content);
  const messageId = message.message_id || raw?.message_id || "";
  return {
    external_event_id: messageId ? `message:${messageId}` : header.event_id || "missing_event_id",
    payload: {
      provider_event_id: header.event_id || "",
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
  if (!value) return {};
  if (typeof value === "object") return value;
  try {
    return JSON.parse(String(value));
  } catch {
    return { text: String(value) };
  }
}

function normalizeMentions(values: any[]) {
  return values.map((mention: any) => ({
    open_id: mention.id?.open_id || mention.open_id || "",
    name: mention.name || mention.id?.name || "",
  }));
}

const eventDispatcher = new Lark.EventDispatcher({}).register({
  "im.message.receive_v1": async (data: unknown) => {
    const normalized = normalizeEvent(data);
    const payload = normalized.payload;

    // 1. Try approval interception (before LLM).
    if (payload.message_type === "text" && payload.text) {
      const handled = await tryHandleApproval(
        payload.text,
        payload.chat_type,
        payload.sender_open_id,
        payload.message_id,
        client,
        reactions,
      ).catch((error) => {
        const message = error instanceof Error ? error.message : String(error);
        console.error(`approval handler error: ${message.slice(0, 200)}`);
        return false;
      });
      if (handled) {
        return; // Approval command handled, skip LLM.
      }
    }

    // 2. Normal flow: send to Kernel / LLM.
    void postIngress(config, data, reactions).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`kernel ingress error: ${message.slice(0, 200)}`);
    });
  },
});

const wsClient = new Lark.WSClient({
  ...baseConfig,
  loggerLevel: Lark.LoggerLevel.info,
});

wsClient.start({ eventDispatcher });
console.log("feishu connector long connection started");
