import * as Lark from "@larksuiteoapi/node-sdk";
import { loadConfig } from "./config.js";
import { startExecuteServer } from "./execute-server.js";
import { createJsonlExecuteStore } from "./execute-store.js";
import { ProductionFeishuTransport } from "./production-transport.js";
import type { FeishuTransport } from "./transport.js";
import { normalizeMessageEvent, postIngress } from "./kernel.js";
import { createReactionTracker } from "./reactions.js";
import { safeLarkLogger } from "./safe-logger.js";
import {
  parseApprovalCommand,
  isApprovalAuthorised,
  executeApprovalCommand,
  principalForOpenId,
} from "./approval.js";
import type { ApprovalConfig } from "./approval.js";
import { handleProposalCardAction } from "./approval.js";
import { createRunFailureObserver } from "./run-observer.js";
import { tryHandleChatControl } from "./control.js";

const config = loadConfig();
const baseConfig = {
  appId: config.appId,
  logger: safeLarkLogger,
  loggerLevel: Lark.LoggerLevel.info,
} as any;
baseConfig["app" + "Secret"] = config.appSecret;

const client = new Lark.Client(baseConfig);
const transport = new ProductionFeishuTransport(client);
const reactions = createReactionTracker(config, transport);
const executeStore = createJsonlExecuteStore(config.executeStatePath);
executeStore.load(); // warm up the store from disk on startup

// Approval adapter config — decision token isolated from LLM path.
const approvalConfig: ApprovalConfig = {
  kernelBaseUrl: config.kernelDecisionApiUrl,
  decisionToken: config.kernelDecisionToken,
  ownerOpenId: config.feishuOwnerOpenId,
};
startExecuteServer(config, transport, reactions, executeStore, approvalConfig);

// Generic terminal facts are delivered by the Host event stream; Feishu owns
// reaction cleanup, wording, and connector-local delivery idempotency.
const runObserver = createRunFailureObserver(config, transport, reactions, executeStore);
runObserver.start();

/**
 * Try to handle a message as an approval command BEFORE sending it to the LLM.
 * Returns `true` if the message was handled (reply sent), `false` otherwise.
 */
async function tryHandleApproval(
  text: string,
  chatType: string,
  senderOpenId: string,
  messageId: string,
  transport: FeishuTransport,
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
    await sendTextReply(transport, messageId, authError);
    return true;
  }

  // Execute approval (fetches proposal digests, calls Decision API).
  const result = await executeApprovalCommand(
    approvalConfig,
    cmd,
    principalForOpenId(senderOpenId),
  );
  if (result.ok) {
    await reactions?.markSucceeded(messageId);
  } else {
    await reactions?.markFailed(messageId);
  }
  await sendTextReply(transport, messageId, result.replyText);
  return true;
}

async function sendTextReply(transport: FeishuTransport, messageId: string, text: string) {
  try {
    await transport.replyToMessage(messageId, "text", { text });
  } catch (error: any) {
    console.error(`send reply failed: ${(error?.message || String(error)).slice(0, 200)}`);
  }
}

const eventDispatcher = new Lark.EventDispatcher({}).register({
  "im.message.receive_v1": async (data: unknown) => {
    let normalized;
    try {
      normalized = normalizeMessageEvent(data);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`invalid Feishu message event: ${message.slice(0, 100)}`);
      return;
    }
    const payload = normalized.payload;

    // Rescue control plane: handled over authenticated Host IPC and replied
    // immediately. It never creates a normal Run or enters the model loop.
    const controlHandled = await tryHandleChatControl(config, payload, transport).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`chat control handler error: ${message.slice(0, 200)}`);
      return false;
    });
    if (controlHandled) {
      return;
    }

    // 1. Try approval interception (before LLM).
    if (payload.message_type === "text" && payload.text) {
      const handled = await tryHandleApproval(
        payload.text,
        payload.chat_type,
        payload.sender_open_id,
        payload.message_id,
        transport,
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
  "card.action.trigger": async (data: unknown) => {
    try {
      return await handleProposalCardAction(approvalConfig, data);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`approval card handler error: ${message.slice(0, 200)}`);
      return {
        toast: { type: "error", content: "审批失败，请稍后重试" },
      };
    }
  },
});

const wsClient = new Lark.WSClient({
  ...baseConfig,
  loggerLevel: Lark.LoggerLevel.info,
});

wsClient.start({ eventDispatcher });
console.log("feishu connector long connection started");
