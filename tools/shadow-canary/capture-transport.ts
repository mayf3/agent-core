/**
 * CaptureFeishuTransport — Shadow Canary transport that captures Feishu
 * API payloads to evidence files instead of calling the real Feishu API.
 *
 * This is the SINGLE authoritative capture transport implementation.
 * It implements the FeishuTransport interface defined in the Connector.
 *
 * Every captured payload includes:
 *   - The full message/card content
 *   - A metadata block with message_id, msg_type, timestamp
 *   - Extracted binding values (proposal_id, approval_id, decision_nonce)
 *     for interactive cards, enabling the downstream callback simulation
 *     to use REAL binding values from the Connector's own card rendering.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import type { FeishuTransport, ReplyResult, ReactionResult } from "../../connectors/feishu/src/transport.js";

export interface CapturedCardPayload {
  /** The message_id this reply was sent to */
  message_id: string;
  /** Message type: "text" | "interactive" */
  msg_type: string;
  /** The full content that would have been sent to Feishu */
  content: unknown;
  /** ISO timestamp of capture */
  captured_at: string;
  /** Extracted binding values (only for interactive/card payloads) */
  bindings?: {
    proposal_id?: string;
    approval_id?: string;
    decision_nonce?: string;
  };
}

export class CaptureFeishuTransport implements FeishuTransport {
  private evidenceDir: string;
  private capturedCards: CapturedCardPayload[] = [];

  constructor(evidenceDir: string) {
    this.evidenceDir = evidenceDir;
    fs.mkdirSync(evidenceDir, { recursive: true });
  }

  async replyToMessage(
    messageId: string,
    msgType: string,
    content: unknown,
  ): Promise<ReplyResult> {
    const capturedAt = new Date().toISOString();

    // Extract bindings from interactive cards
    let bindings: CapturedCardPayload["bindings"] | undefined;
    if (msgType === "interactive" && typeof content === "object" && content !== null) {
      bindings = this.extractCardBindings(content as Record<string, unknown>);
    }

    const payload: CapturedCardPayload = {
      message_id: messageId,
      msg_type: msgType,
      content,
      captured_at: capturedAt,
      bindings,
    };

    this.capturedCards.push(payload);

    // Write to evidence file
    const filename = `card-${messageId}-${Date.now()}.json`;
    fs.writeFileSync(
      path.join(this.evidenceDir, filename),
      JSON.stringify(payload, null, 2),
      "utf-8",
    );

    // Also write a bindings-only file for easy extraction by inject.mjs
    if (bindings && (bindings.proposal_id || bindings.approval_id)) {
      const bindingsFile = `card-bindings-${bindings.proposal_id || "unknown"}.json`;
      fs.writeFileSync(
        path.join(this.evidenceDir, bindingsFile),
        JSON.stringify({ ...bindings, message_id: messageId }, null, 2),
        "utf-8",
      );
    }

    // Return a simulated Feishu API response
    const mockMessageId = `mock_${messageId}_reply_${Date.now()}`;
    return {
      message_id: mockMessageId,
      status: "sent",
    };
  }

  async addReaction(messageId: string, emojiType: string): Promise<ReactionResult> {
    // Simulate successful reaction add
    return {
      reaction_id: `mock_reaction_${messageId}_${emojiType}`,
    };
  }

  async removeReaction(messageId: string, reactionId: string): Promise<void> {
    // Simulate successful reaction removal — noop
  }

  /** Get all captured card payloads for verification */
  getCapturedCards(): CapturedCardPayload[] {
    return [...this.capturedCards];
  }

  /** Find the latest card payload with specific bindings */
  findCardByProposalId(proposalId: string): CapturedCardPayload | undefined {
    return this.capturedCards
      .slice()
      .reverse()
      .find(
        (c) =>
          c.bindings?.proposal_id === proposalId ||
          (c.msg_type === "interactive" &&
            typeof c.content === "object" &&
            c.content !== null &&
            JSON.stringify(c.content).includes(proposalId)),
      );
  }

  /** Write final evidence summary */
  writeEvidenceSummary(snapshotDir: string): void {
    const summary = {
      total_cards_captured: this.capturedCards.length,
      cards: this.capturedCards.map((c) => ({
        message_id: c.message_id,
        msg_type: c.msg_type,
        captured_at: c.captured_at,
        has_bindings: !!c.bindings,
        proposal_id: c.bindings?.proposal_id || null,
      })),
    };
    fs.writeFileSync(
      path.join(snapshotDir, "card-delivery-summary.json"),
      JSON.stringify(summary, null, 2),
      "utf-8",
    );
  }

  /** Extract proposal/approval bindings from a rendered card */
  private extractCardBindings(card: Record<string, unknown>): CapturedCardPayload["bindings"] {
    const bindings: CapturedCardPayload["bindings"] = {};

    // Walk card elements to find action buttons with value bindings
    const elements = card.elements as Array<Record<string, unknown>> | undefined;
    if (!elements) return bindings;

    for (const element of elements) {
      if (element.tag === "action") {
        const actions = element.actions as Array<Record<string, unknown>> | undefined;
        if (!actions) continue;

        for (const action of actions) {
          const value = action.value as Record<string, unknown> | undefined;
          if (!value) continue;

          if (typeof value.proposal_id === "string" && value.proposal_id) {
            bindings.proposal_id = value.proposal_id;
          }
          if (typeof value.approval_id === "string" && value.approval_id) {
            bindings.approval_id = value.approval_id;
          }
          if (typeof value.decision_nonce === "string" && value.decision_nonce) {
            bindings.decision_nonce = value.decision_nonce;
          }
        }
      }
    }

    return bindings;
  }
}
