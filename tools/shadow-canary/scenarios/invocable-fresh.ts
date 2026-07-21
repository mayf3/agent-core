//! InvocableCapability Fresh Shadow — calculator fresh deployment + invoke multiply(6,7)=42.
//!
//! The calculator is invoked by sending the north-star smoke sentence as a
//! Feishu ingress message (calculator_router → calculator_delivery pipeline).
//! There is no POST /v1/invoke HTTP route on the kernel.
//!
//! The result is read from the AssistantReplyDelivered journal event and
//! correlated to the specific invoke run by tracking kernelEventId → runId.
//! No hardcoded output values are accepted.

import { evidence } from "../evidence.ts";
import {
  runDevelopmentCycle,
  getCurrentCursor,
  waitForComponent,
  DevelopmentCycleResult,
} from "../development-cycle.ts";
import {
  kernelRequest,
  sleep,
} from "../clients/http-client.ts";
import { config, simulateFeishuMessage } from "../connector-shadow.ts";

const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const OBSERVE_TOKEN = process.env.AGENT_CORE_EVENT_OBSERVE_TOKEN || "";
const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;
const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";
const COMPONENT_ID = "external.calculator";

/**
 * Given an ingress kernelEventId, find the run_id that was spawned to
 * process it by scanning RunStarted events whose payload matches.
 */
async function findRunForIngress(
  kernelEventId: string,
  timeoutMs: number = 30_000,
): Promise<string | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest("GET", "/v1/events?limit=200", null, OBSERVE_TOKEN);
    if (!resp.ok || !resp.data?.events) {
      await sleep(1_000);
      continue;
    }
    for (const evt of resp.data.events) {
      // RunStarted events carry the triggering event_id in their payload
      if (evt.event_kind === "RunStarted" && evt.payload?.trigger_event_id === kernelEventId) {
        return evt.run_id || null;
      }
    }
    await sleep(1_000);
  }
  return null;
}

/**
 * Wait for an AssistantReplyDelivered event belonging to a specific run,
 * parse the real result text from its payload, and return it as a number.
 * Returns null on timeout.
 */
async function waitForCalculatorResult(
  invokeRunId: string,
  timeoutMs: number = 120_000,
): Promise<number | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest("GET", "/v1/events?limit=100", null, OBSERVE_TOKEN);
    if (!resp.ok || !resp.data?.events) {
      await sleep(1_000);
      continue;
    }
    for (const evt of resp.data.events) {
      if (
        evt.event_kind === "AssistantReplyDelivered" &&
        evt.run_id === invokeRunId
      ) {
        // The payload has the shape: { text: "42" } or { text: 42 }
        const rawText = evt.payload?.text;
        if (rawText === undefined || rawText === null) {
          console.log(`  AssistantReplyDelivered for run ${invokeRunId} has no text field`);
          return null;
        }
        const parsed = typeof rawText === "number" ? rawText : Number(rawText);
        if (!Number.isFinite(parsed)) {
          console.log(`  AssistantReplyDelivered text is not a number: "${rawText}"`);
          return null;
        }
        return parsed;
      }
    }
    await sleep(1_000);
  }
  return null;
}

export async function runInvocableFreshShadow(): Promise<DevelopmentCycleResult | null> {
  console.log(`\n=== INVOCABLE FRESH SHADOW (${RUN_ID}) ===`);
  const startCursor = await getCurrentCursor();

  // ── Phase 1: Development cycle (message → HCR → Proposal → Approval → Activation) ──
  const MESSAGE_TEXT = "开发一个 external.calculator，支持加减乘除";
  const MESSAGE_ID = `invocable_fresh_${RUN_ID}`;

  const result = await runDevelopmentCycle(
    "INVOCABLE_FRESH", MESSAGE_TEXT, MESSAGE_ID, SENDER_OPEN_ID,
    startCursor, COMPONENT_ID, "success",
  );

  if (!result.componentId) {
    evidence.fail("INVOCABLE_FRESH", "development cycle failed");
    return null;
  }

  evidence.write("invocable-fresh-proposal.json", {
    proposal_id: result.proposalId,
    artifact_digest: result.artifactDigest,
    manifest_digest: result.manifestDigest,
    manifest_ref: result.manifestRef,
    activated_manifest_digest: result.activatedManifestDigest,
  });

  // ── Phase 2: Invoke multiply(6,7) via north-star calculator fixture ──
  console.log(`\n[INVOCABLE_FRESH-6] Sending calculator smoke sentence...`);
  const invokeMsgId = `invoke_${RUN_ID}`;
  const invokeResult = await simulateFeishuMessage(
    "用 external.calculator 计算 6 * 7",
    invokeMsgId,
    SENDER_OPEN_ID,
  );

  if (!invokeResult.ok) {
    evidence.fail("INVOCABLE_INVOKE", `smoke sentence ingress failed: ${invokeResult.status}`, invokeResult);
    return null;
  }

  const invokeKernelEventId = invokeResult.kernelEventId || "";
  console.log(`  ingress kernelEventId: ${invokeKernelEventId}`);

  // ── Phase 3: Find the run that processes this invoke ──
  console.log(`[INVOCABLE_FRESH-7] Finding invoke run...`);
  const invokeRunId = await findRunForIngress(invokeKernelEventId);
  if (!invokeRunId) {
    evidence.fail("INVOCABLE_INVOKE", `could not find RunStarted for ingress ${invokeKernelEventId}`);
    return null;
  }
  console.log(`  invoke run_id: ${invokeRunId}`);

  // ── Phase 4: Wait for and verify the calculator result ──
  console.log(`[INVOCABLE_FRESH-8] Waiting for calculator result for run ${invokeRunId}...`);
  const rawResult = await waitForCalculatorResult(invokeRunId);
  if (rawResult === null) {
    evidence.fail("INVOCABLE_INVOKE", `calculator did not return a numeric result for run ${invokeRunId} within timeout`);
    return null;
  }

  console.log(`  raw calculator result: ${rawResult}`);

  // Verify the result is exactly 42
  if (rawResult !== 42) {
    evidence.fail("INVOCABLE_INVOKE", `calculator returned ${rawResult}, expected 42`);
    return null;
  }

  evidence.pass("INVOCABLE_INVOKE", `multiply(6,7) = ${rawResult}`, {
    input: { operation: "multiply", a: 6, b: 7 },
    output: rawResult,
    invoke_message_id: invokeMsgId,
    invoke_kernel_event_id: invokeKernelEventId,
    invoke_run_id: invokeRunId,
  });

  evidence.write("invocable-fresh-invoke.json", {
    invoke_message_id: invokeMsgId,
    invoke_kernel_event_id: invokeKernelEventId,
    invoke_run_id: invokeRunId,
    invoke_input: { operation: "multiply", a: 6, b: 7 },
    invoke_output: rawResult,
    invoke_component: COMPONENT_ID,
  });

  // ── Final verification ──
  evidence.pass("INVOCABLE_FRESH_SHADOW", `invocable fresh shadow passed`, {
    component_id: result.componentId,
    proposal_id: result.proposalId,
    proposal_manifest_digest: result.manifestDigest,
    activated_manifest_digest: result.activatedManifestDigest,
    invoke_message_id: invokeMsgId,
    invoke_run_id: invokeRunId,
    invoke_component: COMPONENT_ID,
    invoke_operation: "multiply",
    invoke_arguments: { a: 6, b: 7 },
    invoke_output: rawResult,
  });

  return result;
}
