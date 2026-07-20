import type { ConnectorConfig } from "./config.js";
import type { FeishuTransport } from "./transport.js";
import {
  createJsonlReactionStore,
  type ReactionStateStore,
  type StoredReactionState,
} from "./reaction-store.js";

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

export function createReactionTracker(
  config: ConnectorConfig,
  transport: FeishuTransport,
  store: ReactionStateStore = createJsonlReactionStore(config.reactionStatePath),
): ReactionTracker {
  const states = loadStates(store);
  const processingEmoji = config.processingReactionEmoji;
  const failedEmoji = config.failedReactionEmoji;
  // Default retry parameters so callers building a minimal ConnectorConfig
  // (e.g. tests) do not have to set every field. A config that explicitly
  // disables retries can set reactionRetryAttempts = 1.
  const retryAttempts = Math.max(1, config.reactionRetryAttempts ?? 3);
  const retryBaseDelayMs = Math.max(0, config.reactionRetryBaseDelayMs ?? 500);
  if (states.size > 0) {
    console.log(`reaction tracker loaded states=${states.size}`);
  }
  return {
    async markProcessing(messageId: string) {
      if (!processingEmoji || !messageId || states.has(messageId)) {
        return;
      }
      if (!transport.addReaction) {
        return;
      }
      try {
        const result = await withRetry(
          () => transport.addReaction!(messageId, processingEmoji),
          retryAttempts,
          retryBaseDelayMs,
          `add processing msg=${shortId(messageId)}`,
        );
        const reactionId = result?.reaction_id;
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
        store.set(storedState(messageId, reactionId, processingEmoji, "processing"));
        console.log(`reaction added emoji=${processingEmoji} msg=${shortId(messageId)} reaction=${shortId(reactionId)}`);
      } catch (error) {
        console.warn(`reaction add failed msg=${shortId(messageId)} category=${errorLabel(error)} attempts=${retryAttempts}`);
      }
    },
    async markSucceeded(messageId: string) {
      await removeTrackedReaction(transport, states, store, messageId, "succeeded", retryAttempts, retryBaseDelayMs);
    },
    async markFailed(messageId: string) {
      if (!messageId || !failedEmoji) {
        return;
      }
      await removeTrackedReaction(transport, states, store, messageId, "failed", retryAttempts, retryBaseDelayMs);
      if (!transport.addReaction) {
        return;
      }
      try {
        const result = await withRetry(
          () => transport.addReaction!(messageId, failedEmoji),
          retryAttempts,
          retryBaseDelayMs,
          `add failed msg=${shortId(messageId)}`,
        );
        const reactionId = result?.reaction_id;
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
        store.set(storedState(messageId, reactionId, failedEmoji, "failed"));
        console.warn(`reaction failed marker added emoji=${failedEmoji} msg=${shortId(messageId)} reaction=${shortId(reactionId)}`);
      } catch (error) {
        console.warn(`reaction failed marker add failed msg=${shortId(messageId)} category=${errorLabel(error)} attempts=${retryAttempts}`);
      }
    },
    async clearProcessing(messageId: string) {
      await removeTrackedReaction(transport, states, store, messageId, "cleared", retryAttempts, retryBaseDelayMs);
    },
  };
}

export function extractReactionId(response: any): string {
  return String(response?.data?.reaction_id || response?.data?.reaction?.reaction_id || "");
}

async function removeTrackedReaction(
  transport: FeishuTransport,
  states: Map<string, ReactionState>,
  store: ReactionStateStore,
  messageId: string,
  reason: string,
  retryAttempts: number,
  retryBaseDelayMs: number,
) {
  const state = states.get(messageId);
  if (!state || state.status === "remove_pending") {
    return;
  }
  state.status = "remove_pending";
  if (!transport.removeReaction) {
    states.delete(messageId);
    store.delete(messageId);
    return;
  }
  try {
    await withRetry(
      () => transport.removeReaction!(messageId, state.reactionId),
      retryAttempts,
      retryBaseDelayMs,
      `remove msg=${shortId(messageId)} reason=${reason}`,
    );
    states.delete(messageId);
    store.delete(messageId);
    console.log(`reaction removed msg=${shortId(messageId)} reaction=${shortId(state.reactionId)} reason=${reason}`);
  } catch (error) {
    state.status = reason === "failed" ? "failed" : "processing";
    console.warn(`reaction remove failed msg=${shortId(messageId)} category=${errorLabel(error)} attempts=${retryAttempts}`);
  }
}

/**
 * Run an async operation with bounded exponential-backoff retries. This is NOT
 * a keepalive loop: total attempts are capped at `attempts` (including the
 * first), preserving the Phase 0 invariant that reaction add/remove is bounded
 * per handled message. A test-injectable sleep keeps the helper deterministic.
 */
export async function withRetry<T>(
  operation: () => Promise<T>,
  attempts: number,
  baseDelayMs: number,
  label: string,
  sleep: (ms: number) => Promise<void> = defaultSleep,
): Promise<T> {
  let lastError: unknown;
  const maxAttempts = Math.max(1, attempts);
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      return await operation();
    } catch (error) {
      lastError = error;
      if (attempt >= maxAttempts) {
        break;
      }
      // Exponential backoff with full jitter: delay in [0, base * 2^(attempt-1)].
      const ceiling = baseDelayMs * 2 ** (attempt - 1);
      const delay = Math.floor(Math.random() * (ceiling + 1));
      console.warn(`reaction retry scheduled label=${label} attempt=${attempt} next_delay_ms=${delay} category=${errorLabel(error)}`);
      await sleep(delay);
    }
  }
  throw lastError;
}

function defaultSleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

function loadStates(store: ReactionStateStore): Map<string, ReactionState> {
  const states = new Map<string, ReactionState>();
  for (const [messageId, state] of store.load()) {
    states.set(messageId, {
      messageId,
      reactionId: state.reactionId,
      emojiType: state.emojiType,
      status: state.status,
    });
  }
  return states;
}

function storedState(
  messageId: string,
  reactionId: string,
  emojiType: string,
  status: StoredReactionState["status"],
): StoredReactionState {
  return {
    messageId,
    reactionId,
    emojiType,
    status,
    updatedAt: new Date().toISOString(),
  };
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
