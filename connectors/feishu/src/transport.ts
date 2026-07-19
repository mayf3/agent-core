/**
 * FeishuTransport — abstract interface for all outbound Feishu API calls.
 *
 * The production implementation wraps the Lark Client (node-sdk).
 * A capture implementation is used by the Shadow Canary to record
 * card payloads without calling the real Feishu API.
 *
 * Every method returns a minimal Promise; implementors may throw on
 * unrecoverable errors or return fallback values for non-critical
 * operations (e.g. reactions).
 */

export interface ReplyResult {
  message_id: string | null;
  status: string;
}

export interface ReactionResult {
  reaction_id: string | null;
}

export interface FeishuTransport {
  /**
   * Reply to a Feishu message (text or interactive card).
   * `msgType` is one of "text" | "interactive".
   * `content` is the body of the message (string for text, object for card).
   */
  replyToMessage(
    messageId: string,
    msgType: string,
    content: unknown,
  ): Promise<ReplyResult>;

  /**
   * Add an emoji reaction to a message.
   */
  addReaction?(messageId: string, emojiType: string): Promise<ReactionResult>;

  /**
   * Remove an emoji reaction from a message.
   */
  removeReaction?(messageId: string, reactionId: string): Promise<void>;
}
