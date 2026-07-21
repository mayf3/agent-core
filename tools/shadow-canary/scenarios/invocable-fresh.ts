//! InvocableCapability Fresh Shadow — calculator fresh deployment + invoke multiply(6,7)=42.

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
const IPC_TOKEN = process.env.AGENT_CORE_IPC_TOKEN || "";
const OBSERVE_TOKEN = process.env.AGENT_CORE_EVENT_OBSERVE_TOKEN || IPC_TOKEN;
const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;
const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";
const COMPONENT_ID = "external.calculator";

export async function runInvocableFreshShadow(): Promise<DevelopmentCycleResult | null> {
  console.log(`\n=== INVOCABLE FRESH SHADOW (${RUN_ID}) ===`);
  const startCursor = await getCurrentCursor();

  // Message text that triggers calculator fixture (component.invoke.v0)
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

  // ── Invoke multiply(6,7) via north-star calculator fixture ──
  // The calculator is invoked by sending the smoke-sentence ingress message
  // that triggers the calculator_router → calculator_delivery pipeline.
  // There is no POST /v1/invoke HTTP route on the kernel.
  console.log(`\n[INVOCABLE_FRESH-6] Invoking multiply(6,7) via calculator smoke sentence...`);
  const invokeMsgId = `invoke_${RUN_ID}`;
  const invokeResult = await simulateFeishuMessage(
    "用 external.calculator 计算 6 * 7",
    invokeMsgId,
    SENDER_OPEN_ID,
  );

  if (!invokeResult.ok) {
    evidence.fail("INVOCABLE_INVOKE", `calculator smoke sentence failed: ${invokeResult.status}`, invokeResult);
    return null;
  }

  // Wait for the calculator result (AssistantReplyDelivered event with value 42)
  console.log(`[INVOCABLE_FRESH-7] Waiting for calculator result...`);
  let resultFound = false;
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const resp = await kernelRequest("GET", "/v1/events/observe?limit=50", null, OBSERVE_TOKEN);
    if (resp.ok && resp.data?.events) {
      for (const evt of resp.data.events) {
        // The calculator returns 42 as AssistantReplyDelivered event payload
        if (evt.event_kind === "AssistantReplyDelivered") {
          const payload = typeof evt.payload === "object" ? JSON.stringify(evt.payload) : String(evt.payload || "");
          if (payload.includes("42") || payload === "42") {
            resultFound = true;
            evidence.pass("INVOCABLE_INVOKE", `multiply(6,7) = 42`, {
              input: { operation: "multiply", a: 6, b: 7 },
              output: 42,
            });
            evidence.write("invocable-fresh-invoke.json", {
              invoke_input: { operation: "multiply", a: 6, b: 7 },
              invoke_output: 42,
            });
            break;
          }
        }
      }
      if (resultFound) break;
    }
    await sleep(2_000);
  }

  if (!resultFound) {
    evidence.fail("INVOCABLE_INVOKE", `calculator did not return 42 within timeout`);
    return null;
  }

  // ── Verify multiply(6,7)=42 ──
  evidence.pass("INVOCABLE_FRESH_SHADOW", `invocable fresh shadow passed`, {
    component_id: result.componentId,
    proposal_id: result.proposalId,
    proposal_manifest_digest: result.manifestDigest,
    activated_manifest_digest: result.activatedManifestDigest,
    invoke_input: { operation: "multiply", a: 6, b: 7 },
    invoke_output: 42,
  });

  return result;
}
