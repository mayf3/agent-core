//! InvocableCapability Fresh Shadow — calculator fresh deployment + invoke multiply(6,7)=42.
//!
//! The calculator is invoked by sending the north-star smoke sentence as a
//! Feishu ingress message (calculator_router → calculator_delivery pipeline).
//! The result is read from the AssistantReplyDelivered journal event,
//! correlated to the specific invoke run by tracking kernelEventId → runId.
//! No hardcoded output values are accepted.

import { evidence } from "../evidence.ts";
import {
  runDevelopmentCycle,
  getCurrentCursor,
  invokeCalculator,
  DevelopmentCycleResult,
} from "../development-cycle.ts";
import { config } from "../connector-shadow.ts";

const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;
const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";
const COMPONENT_ID = "external.calculator";

export async function runInvocableFreshShadow(): Promise<DevelopmentCycleResult | null> {
  console.log(`\n=== INVOCABLE FRESH SHADOW (${RUN_ID}) ===`);
  const startCursor = await getCurrentCursor();

  // Phase 1: Development cycle
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

  // Phase 2: Invoke and verify
  console.log(`\n[INVOCABLE_FRESH-6] Invoking multiply(6,7) via calculator smoke sentence...`);
  const invokeMsgId = `invoke_${RUN_ID}`;

  const calcResult = await invokeCalculator(invokeMsgId, SENDER_OPEN_ID);
  if (!calcResult.ok) {
    evidence.fail("INVOCABLE_INVOKE", calcResult.error || "unknown error");
    return null;
  }

  const rawResult = calcResult.result!;
  console.log(`  raw calculator result: ${rawResult}, run_id: ${calcResult.runId}`);

  if (rawResult !== 42) {
    evidence.fail("INVOCABLE_INVOKE", `calculator returned ${rawResult}, expected 42`);
    return null;
  }

  evidence.pass("INVOCABLE_INVOKE", `multiply(6,7) = ${rawResult}`, {
    input: { operation: "multiply", a: 6, b: 7 },
    output: rawResult,
    invoke_message_id: invokeMsgId,
    invoke_run_id: calcResult.runId,
  });

  evidence.write("invocable-fresh-invoke.json", {
    invoke_message_id: invokeMsgId,
    invoke_run_id: calcResult.runId,
    invoke_input: { operation: "multiply", a: 6, b: 7 },
    invoke_output: rawResult,
    invoke_component: COMPONENT_ID,
  });

  evidence.pass("INVOCABLE_FRESH_SHADOW", `invocable fresh shadow passed`, {
    component_id: result.componentId,
    proposal_id: result.proposalId,
    proposal_manifest_digest: result.manifestDigest,
    activated_manifest_digest: result.activatedManifestDigest,
    invoke_message_id: invokeMsgId,
    invoke_run_id: calcResult.runId,
    invoke_component: COMPONENT_ID,
    invoke_operation: "multiply",
    invoke_arguments: { a: 6, b: 7 },
    invoke_output: rawResult,
  });

  return result;
}
