//! InvocableCapability Fresh Shadow — calculator fresh deployment + invoke multiply(6,7)=42.

import { evidence } from "../evidence.ts";
import {
  runDevelopmentCycle,
  getCurrentCursor,
  waitForComponent,
  loopbackRequest,
  DevelopmentCycleResult,
} from "../development-cycle.ts";
import {
  kernelRequest,
  sleep,
} from "../clients/http-client.ts";
import { config } from "../connector-shadow.ts";

const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const IPC_TOKEN = process.env.AGENT_CORE_IPC_TOKEN || "";
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

  // ── Invoke multiply(6,7) via capability host ──
  console.log(`\n[INVOCABLE_FRESH-6] Invoking multiply(6,7)...`);
  const IPC_TOKEN = process.env.AGENT_CORE_IPC_TOKEN || "";
  const invokeResult = await invokeCapability(
    "external.calculator",
    "external.calculator",
    { operation: "multiply", a: 6, b: 7 },
  );

  if (invokeResult.ok && invokeResult.data?.result === 42) {
    evidence.pass("INVOCABLE_INVOKE", `multiply(6,7) = ${invokeResult.data.result}`, {
      input: { operation: "multiply", a: 6, b: 7 },
      output: invokeResult.data.result,
      expected: 42,
    });
    evidence.write("invocable-fresh-invoke.json", {
      invoke_input: { operation: "multiply", a: 6, b: 7 },
      invoke_output: invokeResult.data,
    });
  } else {
    evidence.fail("INVOCABLE_INVOKE", `multiply(6,7) failed: ${JSON.stringify(invokeResult)}`, invokeResult);
    return result;
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

/** Invoke a capability through the Kernel's invoke API. */
async function invokeCapability(
  componentId: string,
  operation: string,
  args: any,
): Promise<any> {
  const body = {
    protocol_version: "process-harness-v1",
    operation,
    component_id: componentId,
    arguments: args,
  };
  return kernelRequest("POST", "/v1/invoke", body, IPC_TOKEN);
}
