//! HookConsumer Fresh Shadow — failure-viewer fresh deployment.

import { evidence } from "../evidence.ts";
import {
  runDevelopmentCycle,
  getCurrentCursor,
  disableComponent,
  waitForComponent,
  injectShadowMarker,
  waitForMarkerIngested,
  DevelopmentCycleResult,
} from "../development-cycle.ts";
import {
  kernelRequest,
  sleep,
} from "../clients/http-client.ts";
import { config } from "../connector-shadow.ts";

const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;

export async function runHookFreshShadow(): Promise<void> {
  console.log(`\n=== HOOK FRESH SHADOW (${RUN_ID}) ===`);

  const startCursor = await getCurrentCursor();
  const MESSAGE_TEXT = "开发一个failure-viewer，event.observe.v0";
  const MESSAGE_ID = `hook_fresh_${RUN_ID}`;
  const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";
  const COMPONENT_ID = "failure-viewer";

  const result = await runDevelopmentCycle(
    "FRESH", MESSAGE_TEXT, MESSAGE_ID, SENDER_OPEN_ID,
    startCursor, COMPONENT_ID, "success",
  );

  if (!result.componentId) {
    return;
  }

  // Step 8: Inject shadow marker
  console.log(`\n[FRESH-8] Injecting shadow marker...`);
  const markerResult = await injectShadowMarker(RUN_ID, "hook_fresh");
  if (!markerResult.ok) {
    evidence.fail("INJECT_MARKER", `marker injection failed`, markerResult);
    return;
  }
  evidence.pass("INJECT_MARKER", `marker injected for ${RUN_ID}`, markerResult);

  // Wait for marker ingestion
  const markerId = `shadow_marker_${RUN_ID}_hook_fresh`;
  try {
    await waitForMarkerIngested(markerId, 120_000);
    evidence.pass("MARKER_INGESTED", `marker ${markerId} ingested`, { marker_id: markerId });
  } catch (err: any) {
    evidence.fail("MARKER_INGESTED", `marker ingestion timeout: ${err.message}`);
  }

  // Step 9: Disable component
  console.log(`\n[FRESH-9] Disabling component...`);
  if (!result.componentSnapshotId) {
    // Look up component
    try {
      const compResp = await kernelRequest(
        "GET", `/v1/components/${COMPONENT_ID}`, null, DECISION_TOKEN,
      );
      if (compResp.ok && compResp.data?.component_snapshot_id) {
        result.componentSnapshotId = compResp.data.component_snapshot_id;
        result.deploymentId = result.deploymentId || compResp.data.component?.deployment_id || "";
      }
    } catch {}
  }
  const disableResult = await disableComponent(
    COMPONENT_ID,
    result.componentSnapshotId || "",
    result.deploymentId || "",
  );
  if (!disableResult.ok) {
    evidence.fail("DISABLE", `disable ${COMPONENT_ID} failed`, disableResult);
    return;
  }
  evidence.pass("DISABLE", `component ${COMPONENT_ID} disabled`, {
    component_status: disableResult.data?.component_status,
  });

  evidence.pass("HOOK_FRESH_SHADOW", `hook fresh shadow completed`, {
    component_id: result.componentId,
    version: result.version,
    manifest_digest: result.manifestDigest,
    activated_manifest_digest: result.activatedManifestDigest,
  });
}
