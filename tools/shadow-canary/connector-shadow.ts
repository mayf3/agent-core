/**
 * connector-shadow.ts — Shadow Canary Connector Entry Point
 *
 * This is an INDEPENDENT entry point for the Feishu Connector that:
 * - Uses PRODUCTION Connector modules (config, kernel, approval, execute-server)
 * - Does NOT create a Lark WSClient (no real Feishu WebSocket connection)
 * - Does NOT use ProductionFeishuTransport
 * - Injects CaptureFeishuTransport to capture outgoing card payloads
 *
 * This script is launched by canary-runtime shadow-e2e with the shadow.env.
 * It provides hook functions for inject.mjs to simulate Feishu platform events.
 *
 * Usage:
 *   npx tsx tools/shadow-canary/connector-shadow.ts
 */

import { loadConfig } from "../../connectors/feishu/src/config.js";
import { startExecuteServer } from "../../connectors/feishu/src/execute-server.js";
import {
  normalizeMessageEvent,
  postIngress,
} from "../../connectors/feishu/src/kernel.js";
import {
  handleProposalCardAction,
} from "../../connectors/feishu/src/approval.js";
import type { ApprovalConfig } from "../../connectors/feishu/src/approval.js";
import { CaptureFeishuTransport } from "./capture-transport.js";
import * as fs from "node:fs";
import * as path from "node:path";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const EVIDENCE_DIR = process.env.SHADOW_EVIDENCE_DIR || "/tmp/agent-core-shadow-evidence";

// Ensure evidence directory exists
fs.mkdirSync(EVIDENCE_DIR, { recursive: true });

// Load configuration from shadow.env (loaded by canary-runtime)
const config = loadConfig();

// Create the capture transport — intercepts all Feishu API calls
const transport = new CaptureFeishuTransport(EVIDENCE_DIR);

// Approval adapter config
const approvalConfig: ApprovalConfig = {
  kernelBaseUrl: config.kernelDecisionApiUrl,
  decisionToken: config.kernelDecisionToken,
  ownerOpenId: config.feishuOwnerOpenId,
};

// ---------------------------------------------------------------------------
// Shadow-specific execute store (no production state contamination)
// ---------------------------------------------------------------------------

const EXECUTE_STATE_PATH = path.join(
  process.env.SHADOW_STATE_DIR || "/tmp/agent-core-shadow-state",
  "feishu-executes-shadow.jsonl",
);

// Minimal in-memory execute store for the shadow
const shadowExecuteStore = {
  _data: new Map<string, { status: string; receiptSummary?: { messageId: string | null } }>(),
  load() {
    try {
      const text = fs.readFileSync(EXECUTE_STATE_PATH, "utf-8");
      for (const line of text.trim().split("\n").filter(Boolean)) {
        const entry = JSON.parse(line);
        this._data.set(entry.idempotencyKey, entry);
      }
    } catch {
      // File doesn't exist yet — that's fine
    }
  },
  get(key: string) {
    return this._data.get(key) || null;
  },
  set(entry: any) {
    this._data.set(entry.idempotencyKey, entry);
    fs.appendFileSync(EXECUTE_STATE_PATH, JSON.stringify(entry) + "\n");
  },
};

shadowExecuteStore.load();

// ---------------------------------------------------------------------------
// Start the execute server (production code path)
// ---------------------------------------------------------------------------

const _server = startExecuteServer(config, transport, undefined, shadowExecuteStore as any, approvalConfig);

console.log(`[shadow-connector] listening on port ${config.connectorPort}`);
console.log(`[shadow-connector] kernel URL: ${config.kernelUrl}`);
console.log(`[shadow-connector] evidence dir: ${EVIDENCE_DIR}`);
console.log(`[shadow-connector] WSClient NOT started (shadow mode)`);

// Export a close function so inject.ts can cleanly stop the server
// and release the port before exiting. Without this, the node process
// stays alive (holding port 4131) after the test completes, blocking
// subsequent shadow runs.
export function closeServer(): void {
  _server.close();
}

// ---------------------------------------------------------------------------
// Export injection hooks for inject.mjs
// ---------------------------------------------------------------------------

export { transport, approvalConfig, config };

/**
 * Simulate a Feishu message event being received by the Connector.
 *
 * Builds a fake im.message.receive_v1 event and passes it through the
 * Connector's PRODUCTION normalizeMessageEvent + postIngress pipeline.
 *
 * @param text - The message text (e.g., "开发一个 failure-viewer...")
 * @param messageId - Unique message ID for dedup
 * @param senderOpenId - The sender's open_id (must match owner)
 * @returns The ingress response
 */
export async function simulateFeishuMessage(
  text: string,
  messageId: string,
  senderOpenId?: string,
): Promise<{ ok: boolean; status?: number; kernelEventId?: string }> {
  const openId = senderOpenId || config.feishuOwnerOpenId || "ou_shadow_default";

  // Build a fake im.message.receive_v1 event
  const fakeFeishuEvent = {
    header: {
      event_id: `shadow_event_${messageId}`,
    },
    event: {
      sender: {
        sender_id: { open_id: openId },
        sender_type: "user",
      },
      message: {
        message_id: messageId,
        message_type: "text",
        chat_id: "oc_shadow_chat",
        chat_type: "p2p",
        content: JSON.stringify({ text }),
        mentions: [],
      },
    },
  };

  try {
    // Step 1: Normalize the event (PRODUCTION code)
    const normalized = normalizeMessageEvent(fakeFeishuEvent);
    console.log(`[shadow] normalized message: type=${normalized.payload.message_type} chat=${normalized.payload.chat_type}`);

    // Step 2: POST to Kernel /v1/ingress via fetch (same endpoint as postIngress,
    // but captures the HTTP response which postIngress discards)
    const ingressBody = {
      protocol_version: "v1",
      source: "Feishu",
      external_event_id: normalized.external_event_id,
      received_at: new Date().toISOString(),
      payload: normalized.payload,
      routing_hint: {},
      auth_context: { authenticated: true },
    };

    // Retry up to 5 times with exponential backoff
    let lastError: any = null;
    for (let attempt = 0; attempt < 5; attempt++) {
      if (attempt > 0) {
        const delay = Math.min(1000 * Math.pow(2, attempt - 1), 10_000);
        console.log(`[shadow] retrying ingress in ${delay}ms (attempt ${attempt + 1})...`);
        await new Promise(r => setTimeout(r, delay));
      }
      try {
        const response = await fetch(config.kernelUrl, {
          method: "POST",
          headers: {
            Authorization: `Bearer ${config.ipcToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify(ingressBody),
          signal: AbortSignal.timeout(30_000),
        });

        let data: any = {};
        try { data = await response.json(); } catch { /* ignore parse errors */ }

        console.log(`[shadow] ingress response: HTTP ${response.status} status=${data.status || "unknown"}`);

        return {
          ok: response.ok,
          status: response.status,
          kernelEventId: data.kernel_event_id || data.run_id || "",
        };
      } catch (error: any) {
        lastError = error;
        console.error(`[shadow] simulateFeishuMessage attempt ${attempt + 1} error: ${error.message}`);
      }
    }
    console.error(`[shadow] simulateFeishuMessage all retries exhausted: ${lastError.message}`);
    return { ok: false };
  } catch (error: any) {
    console.error(`[shadow] simulateFeishuMessage unexpected error: ${error.message}`);
    return { ok: false };
  }

/**
 * Simulate a card approval callback being received by the Connector.
 *
 * Uses the CAPTURED card payload from CaptureFeishuTransport to extract
 * REAL proposal_id, approval_id, and decision_nonce values. This ensures
 * the simulated callback uses the exact same bindings that the Connector
 * put into the card, catching mismatches.
 *
 * @param proposalId - The proposal_id to approve (looks up captured card)
 * @returns The card callback response
 */
export async function simulateCardApproval(
  proposalId: string,
): Promise<{ ok: boolean; toast?: string }> {
  // Find the captured card for this proposal
  const cardPayload = transport.findCardByProposalId(proposalId);

  if (!cardPayload || !cardPayload.bindings) {
    console.error(`[shadow] no captured card found for proposal ${proposalId}`);
    return { ok: false, toast: "captured_card_not_found" };
  }

  const { approval_id, decision_nonce } = cardPayload.bindings;
  if (!approval_id || !decision_nonce) {
    console.error(`[shadow] captured card for ${proposalId} missing bindings`);
    return { ok: false, toast: "card_missing_bindings" };
  }

  console.log(`[shadow] card approval using REAL bindings from captured card:`);
  console.log(`  proposal_id: ${proposalId}`);
  console.log(`  approval_id: ${approval_id}`);
  console.log(`  decision_nonce: ${decision_nonce}`);

  // Build a fake card.action.trigger event with the REAL bindings
  const fakeCardEvent = {
    event: {
      operator: {
        open_id: config.feishuOwnerOpenId || "ou_shadow_default",
      },
      action: {
        value: {
          proposal_id: proposalId,
          approval_id,
          decision_nonce,
          decision: "approved",
        },
      },
    },
  };

  try {
    // Call the production handleProposalCardAction (production code)
    const result = await handleProposalCardAction(approvalConfig, fakeCardEvent);
    console.log(`[shadow] card callback result: toast=${result?.toast?.content} type=${result?.toast?.type}`);
    return {
      ok: result?.toast?.type === "success",
      toast: result?.toast?.content,
    };
  } catch (error: any) {
    console.error(`[shadow] simulateCardApproval error: ${error.message}`);
    return { ok: false, toast: error.message };
  }
}

/**
 * Wait for the Kernel to create a proposal with the expected status.
 * Polls the Kernel's proposal query endpoint.
 */
export async function waitForProposal(
  proposalId: string,
  timeoutMs: number = 180_000,
): Promise<{ ok: boolean; status?: string }> {
  const url = `${config.kernelDecisionApiUrl}/v1/capability-change-proposals/${proposalId}`;
  const headers = { Authorization: `Bearer ${config.kernelDecisionToken}` };
  const startTime = Date.now();

  while (Date.now() - startTime < timeoutMs) {
    try {
      const response = await fetch(url, { headers });
      if (response.ok) {
        const data = await response.json();
        return { ok: true, status: data.status };
      }
      // Not found yet — keep polling
    } catch {
      // Connection refused — kernel may not be ready
    }
    await sleep(1_000);
  }

  return { ok: false, status: "timeout" };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
