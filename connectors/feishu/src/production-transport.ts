/**
 * ProductionFeishuTransport — wraps a Lark Client and calls the real Feishu API.
 *
 * This is the default transport used in production (index.ts).
 * It delegates every call to the Lark node-sdk client.request().
 */

import type { FeishuTransport, ReplyResult, ReactionResult } from "./transport.js";

export class ProductionFeishuTransport implements FeishuTransport {
  constructor(private readonly client: any) {}

  async replyToMessage(
    messageId: string,
    msgType: string,
    content: unknown,
  ): Promise<ReplyResult> {
    const payload =
      msgType === "interactive"
        ? { msg_type: msgType, content: JSON.stringify(content) }
        : { msg_type: msgType, content: JSON.stringify(content) };
    const response = await this.client.request({
      method: "POST",
      url: `/open-apis/im/v1/messages/${encodeURIComponent(messageId)}/reply`,
      data: payload,
    });
    return {
      message_id:
        response?.data?.message_id ||
        response?.data?.message?.message_id ||
        null,
      status: "sent",
    };
  }

  async addReaction(
    messageId: string,
    emojiType: string,
  ): Promise<ReactionResult> {
    const response = await this.client.request({
      method: "POST",
      url: `/open-apis/im/v1/messages/${encodeURIComponent(messageId)}/reactions`,
      data: {
        reaction_type: { emoji_type: emojiType },
      },
    });
    return {
      reaction_id:
        response?.data?.reaction_id ||
        response?.data?.reaction?.reaction_id ||
        null,
    };
  }

  async removeReaction(messageId: string, reactionId: string): Promise<void> {
    await this.client.request({
      method: "DELETE",
      url: `/open-apis/im/v1/messages/${encodeURIComponent(messageId)}/reactions/${encodeURIComponent(reactionId)}`,
    });
  }
}
