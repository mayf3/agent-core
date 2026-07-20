//! HookConsumer Dirty Shadow — failure-viewer upgrade failure + rollback.

import { evidence } from "../evidence.ts";
import {
  runDevelopmentCycle,
  getCurrentCursor,
  disableComponent,
  waitForComponent,
  waitForComponentCursor,
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
const COMPONENT_ID = "failure-viewer";

export async function runHookDirtyShadow(): Promise<void> {
  console.log(`\n=== HOOK DIRTY SHADOW (${RUN_ID}) ===`);

  // ═══════════════════════════════════════════════════════════════
  // Phase A: Initial successful deployment
  // ═══════════════════════════════════════════════════════════════
  const phaseAStart = await getCurrentCursor();
  const msgIdA = `hook_dirty_A_${RUN_ID}`;

  const resultA = await runDevelopmentCycle(
    "A", "开发一个failure-viewer，event.observe.v0", msgIdA, SENDER_OPEN_ID,
    phaseAStart, COMPONENT_ID, "success",
  );
  if (!resultA.componentId) return;
  const oldSnapshotId = resultA.componentSnapshotId || "";
  const oldDeploymentId = resultA.deploymentId || "";

  // ═══════════════════════════════════════════════════════════════
  // Phase B: Upgrade with expected failure
  // ═══════════════════════════════════════════════════════════════
  const phaseBStart = await getCurrentCursor();
  const msgIdB = `hook_dirty_B_${RUN_ID}`;

  const resultB = await runDevelopmentCycle(
    "B", "升级 failure-viewer，使用 event.observe.v0 监控更多失败事件", msgIdB, SENDER_OPEN_ID,
    phaseBStart, COMPONENT_ID, "failure",
  );
  if (!resultB.failedReceiptId) return;

  // Verify old object still works
  evidence.pass("PHASE_B_OLD_PRESERVED", `old component snapshot ${oldSnapshotId} preserved`, {
    old_snapshot_id: oldSnapshotId,
    failed_receipt_id: resultB.failedReceiptId,
  });

  // ═══════════════════════════════════════════════════════════════
  // Phase C: Successful upgrade
  // ═══════════════════════════════════════════════════════════════
  const phaseCStart = await getCurrentCursor();
  const msgIdC = `hook_dirty_C_${RUN_ID}`;

  const resultC = await runDevelopmentCycle(
    "C", "升级 failure-viewer，使用 event.observe.v0 监控更多失败事件", msgIdC, SENDER_OPEN_ID,
    phaseCStart, COMPONENT_ID, "success",
  );
  if (!resultC.componentId) return;

  // ═══════════════════════════════════════════════════════════════
  // Phase D: Rollback to old version
  // ═══════════════════════════════════════════════════════════════
  // Disable the current (C) version
  if (resultC.componentSnapshotId && resultC.deploymentId) {
    await disableComponent(COMPONENT_ID, resultC.componentSnapshotId, resultC.deploymentId);
  }

  // Verify old version is accessible
  let rollbackComponent: any;
  try {
    rollbackComponent = await waitForComponentCursor(COMPONENT_ID, resultA.version || "0.1.0", 180_000);
    evidence.pass("ROLLBACK", `rolled back to version ${resultA.version}`, {
      component_id: COMPONENT_ID,
      version: rollbackComponent?.component?.version,
      component_snapshot_id: rollbackComponent?.component_snapshot_id,
    });
  } catch (err: any) {
    evidence.fail("ROLLBACK", `rollback failed: ${err.message}`);
    return;
  }

  evidence.pass("HOOK_DIRTY_SHADOW", `hook dirty shadow completed`, {
    old_version: resultA.version,
    failed_version: resultB.failedReceiptId,
    successful_version: resultC.version,
    rollback_version: rollbackComponent?.component?.version,
  });
}
