import type { ConnectorConfig } from "./config.js";

export interface ReactionTracker {
  markProcessing(messageId: string): Promise<void>;
  markSucceeded(messageId: string): Promise<void>;
  markFailed(messageId: string): Promise<void>;
  clearProcessing(messageId: string): Promise<void>;
}

type ReactionState = {
  messageId: string;
  reactionId: string;
  emojiType: string;
  status: "processing" | "failed" | "remove_pending";
};

export function createReactionTracker(config: ConnectorConfig, client: any): ReactionTracker {
  const states = new Map<string, ReactionState>();
  const processingEmoji = config.processingReactionEmoji;
  const failedEmoji = config.failedReactionEmoji;
  return {
    async markProcessing(messageId: string) {
      if (!processingEmoji || !messageId || states.has(messageId)) {
        return;
      }
      try {
        const reactionId = await addReaction(client, messageId, processingEmoji);
        if (!reactionId) {
          console.log(`reaction add skipped msg=${shortId(messageId)} reason=no_reaction_id`);
          return;
        }
        states.set(messageId, {
          messageId,
          reactionId,
          emojiType: processingEmoji,
          status: "processing",
        });
        console.log(`reaction added emoji=${processingEmoji} msg=${shortId(messageId)} reaction=${shortId(reactionId)}`);
      } catch (error) {
        console.warn(`reaction add failed msg=${shortId(messageId)} category=${errorLabel(error)}`);
      }
    },
    async markSucceeded(messageId: string) {
      await removeTrackedReaction(client, states, messageId, "succeeded");
    },
    async markFailed(messageId: string) {
      if (!messageId || !failedEmoji) {
        return;
      }
      await removeTrackedReaction(client, states, messageId, "failed");
      try {
        const reactionId = await addReaction(client, messageId, failedEmoji);
        if (!reactionId) {
          console.warn(`reaction failed marker skipped msg=${shortId(messageId)} reason=no_reaction_id`);
          return;
        }
        states.set(messageId, {
          messageId,
          reactionId,
          emojiType: failedEmoji,
          status: "failed",
        });
        console.warn(`reaction failed marker added emoji=${failedEmoji} msg=${shortId(messageId)} reaction=${shortId(reactionId)}`);
      } catch (error) {
        console.warn(`reaction failed marker add failed msg=${shortId(messageId)} category=${errorLabel(error)}`);
      }
    },
    async clearProcessing(messageId: string) {
      await removeTrackedReaction(client, states, messageId, "cleared");
    },
  };
}

export function extractReactionId(response: any): string {
  return String(response?.data?.reaction_id || response?.data?.reaction?.reaction_id || "");
}

async function addReaction(client: any, messageId: string, emojiType: string): Promise<string> {
  const response = await client.request({
    method: "POST",
    url: `/open-apis/im/v1/messages/${encodeURIComponent(messageId)}/reactions`,
    data: {
      reaction_type: {
        emoji_type: emojiType,
      },
    },
  });
  return extractReactionId(response);
}

async function removeTrackedReaction(
  client: any,
  states: Map<string, ReactionState>,
  messageId: string,
  reason: string,
) {
  const state = states.get(messageId);
  if (!state || state.status === "remove_pending") {
    return;
  }
  state.status = "remove_pending";
  try {
    await client.request({
      method: "DELETE",
      url: `/open-apis/im/v1/messages/${encodeURIComponent(messageId)}/reactions/${encodeURIComponent(state.reactionId)}`,
    });
    states.delete(messageId);
    console.log(`reaction removed msg=${shortId(messageId)} reaction=${shortId(state.reactionId)} reason=${reason}`);
  } catch (error) {
    state.status = reason === "failed" ? "failed" : "processing";
    console.warn(`reaction remove failed msg=${shortId(messageId)} category=${errorLabel(error)}`);
  }
}

function errorLabel(error: any) {
  const code = error?.code || error?.status || error?.response?.status;
  if (code) {
    return `code_${code}`;
  }
  const message = String(error?.message || error?.name || "request_failed").toLowerCase();
  if (message.includes("timeout")) {
    return "timeout";
  }
  if (message.includes("permission") || message.includes("unauthorized")) {
    return "permission";
  }
  return "request_failed";
}

function shortId(value: string) {
  if (!value) {
    return "-";
  }
  return value.length <= 10 ? value : `${value.slice(0, 4)}...${value.slice(-4)}`;
}
