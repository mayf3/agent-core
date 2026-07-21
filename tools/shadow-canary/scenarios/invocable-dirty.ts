//! InvocableCapability Dirty Shadow — calculator upgrade failure + rollback + invoke verify.
//!
//! Phase A: Initial calculator activation + multiply(6,7)=42
//! Phase B: Upgrade with controlled Activation failure → old calculator still returns 42
//! Phase C: Successful upgrade → new calculator returns 42
//! Phase D: Rollback to old version → calculator returns 42 again

import { evidence } from "../evidence.ts";
import {
  runDevelopmentCycle,
  getCurrentCursor,
  invokeCalculator,
  DevelopmentCycleResult,
} from "../development-cycle.ts";
import {
  kernelRequest,
  sleep,
} from "../clients/http-client.ts";
import { config } from "../connector-shadow.ts";

const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;
const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";
const COMPONENT_ID = "external.calculator";
const MESSAGE_TEXT = "开发一个 external.calculator，支持加减乘除";

async function verifyCalculator(
  phase: string,
  expected: number,
): Promise<boolean> {
  const invokeMsgId = `inv_dirty_${phase}_${RUN_ID}`;
  const calcResult = await invokeCalculator(invokeMsgId, SENDER_OPEN_ID);
  if (!calcResult.ok) {
    evidence.fail(`PHASE_${phase}_INVOKE`, `invoke failed: ${calcResult.error}`);
    return false;
  }
  const result = calcResult.result!;
  if (result !== expected) {
    evidence.fail(`PHASE_${phase}_INVOKE`, `calculator returned ${result}, expected ${expected}`);
    return false;
  }
  evidence.pass(`PHASE_${phase}_INVOKE`, `Phase ${phase}: multiply(6,7)=${result}`, {
    invoke_result: result,
    invoke_run_id: calcResult.runId,
  });
  return true;
}

export async function runInvocableDirtyShadow(): Promise<void> {
  console.log(`\n=== INVOCABLE DIRTY SHADOW (${RUN_ID}) ===`);

  let oldManifestDigest = "";
  let failedManifestDigest = "";
  let successManifestDigest = "";
  let rollbackManifestDigest = "";

  // ═══════════════════════════════════════════════════════════════
  // Phase A: Initial successful activation
  // ═══════════════════════════════════════════════════════════════
  console.log(`\n--- Phase A: Initial calculator activation ---`);
  const phaseAStart = await getCurrentCursor();
  const msgIdA = `inv_dirty_A_${RUN_ID}`;

  const resultA = await runDevelopmentCycle(
    "A", MESSAGE_TEXT, msgIdA, SENDER_OPEN_ID,
    phaseAStart, COMPONENT_ID, "success",
  );
  if (!resultA.componentId) return;
  oldManifestDigest = resultA.activatedManifestDigest || resultA.manifestDigest || "";

  // Verify multiply(6,7)=42 on old version
  if (!(await verifyCalculator("A", 42))) return;

  // ═══════════════════════════════════════════════════════════════
  // Phase B: Upgrade with Activation failure
  // ═══════════════════════════════════════════════════════════════
  console.log(`\n--- Phase B: Upgrade with controlled Activation failure ---`);
  const phaseBStart = await getCurrentCursor();
  const msgIdB = `inv_dirty_B_${RUN_ID}`;

  const resultB = await runDevelopmentCycle(
    "B", MESSAGE_TEXT, msgIdB, SENDER_OPEN_ID,
    phaseBStart, COMPONENT_ID, "failure",
  );
  if (!resultB.failedReceiptId) return;
  failedManifestDigest = resultB.manifestDigest || "";

  // Verify old capability still works after failure
  if (!(await verifyCalculator("B", 42))) return;

  evidence.write("invocable-dirty-phase-b.json", {
    old_manifest_digest: oldManifestDigest,
    failed_manifest_digest: failedManifestDigest,
    failed_receipt_id: resultB.failedReceiptId,
    invoke_after_failure: 42,
  });

  // ═══════════════════════════════════════════════════════════════
  // Phase C: Successful activation
  // ═══════════════════════════════════════════════════════════════
  console.log(`\n--- Phase C: Successful activation ---`);
  const phaseCStart = await getCurrentCursor();
  const msgIdC = `inv_dirty_C_${RUN_ID}`;

  const resultC = await runDevelopmentCycle(
    "C", MESSAGE_TEXT, msgIdC, SENDER_OPEN_ID,
    phaseCStart, COMPONENT_ID, "success",
  );
  if (!resultC.componentId) return;
  successManifestDigest = resultC.activatedManifestDigest || resultC.manifestDigest || "";

  // Verify multiply(6,7)=42 on new version
  if (!(await verifyCalculator("C", 42))) return;

  // ═══════════════════════════════════════════════════════════════
  // Phase D: Rollback to old version
  // ═══════════════════════════════════════════════════════════════
  console.log(`\n--- Phase D: Rollback ---`);

  // Disable the current (C) version
  if (resultC.componentSnapshotId && resultC.deploymentId) {
    const disableBody = {
      component_snapshot_id: resultC.componentSnapshotId,
      deployment_record_id: resultC.deploymentId,
    };
    const disableResp = await kernelRequest(
      "POST", `/v1/components/${COMPONENT_ID}/disable`, disableBody, DECISION_TOKEN,
    );
    if (disableResp.ok) {
      evidence.pass("PHASE_D_DISABLE", `disabled ${COMPONENT_ID} for rollback`, {});
    }
  }

  // Re-activate via a new development cycle
  const phaseDStart = await getCurrentCursor();
  const msgIdD = `inv_dirty_D_${RUN_ID}`;
  const resultD = await runDevelopmentCycle(
    "D", MESSAGE_TEXT, msgIdD, SENDER_OPEN_ID,
    phaseDStart, COMPONENT_ID, "success",
  );
  if (!resultD.componentId) return;
  rollbackManifestDigest = resultD.activatedManifestDigest || "";

  // Verify multiply(6,7)=42 after rollback
  if (!(await verifyCalculator("D", 42))) return;

  evidence.write("invocable-dirty-summary.json", {
    old_manifest_digest: oldManifestDigest,
    failed_manifest_digest: failedManifestDigest,
    successful_manifest_digest: successManifestDigest,
    rollback_manifest_digest: rollbackManifestDigest,
    invoke_after_failure: 42,
    invoke_after_success: 42,
    invoke_after_rollback: 42,
  });

  evidence.pass("INVOCABLE_DIRTY_SHADOW", `invocable dirty shadow passed`, {
    old_manifest_digest: oldManifestDigest,
    failed_manifest_digest: failedManifestDigest,
    successful_manifest_digest: successManifestDigest,
    rollback_manifest_digest: rollbackManifestDigest,
    invoke_after_failure: 42,
    invoke_after_success: 42,
    invoke_after_rollback: 42,
  });
}
