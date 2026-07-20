//! Shared development cycle — runs the full flow from message to deployment/activation.

import { kernelRequest, sleep } from "./clients/http-client.ts";
import { evidence } from "./evidence.ts";
import {
  simulateFeishuMessage,
  simulateCardApproval,
  waitForProposal,
  config,
} from "./connector-shadow.ts";
import type { CapturedCardPayload } from "./capture-transport.ts";

const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const IPC_TOKEN = process.env.AGENT_CORE_IPC_TOKEN || "";
const KERNEL_PORT = parseInt(process.env.AGENT_CORE_KERNEL_PORT || "4130", 10);
const KERNEL_BASE = `http://127.0.0.1:${KERNEL_PORT}`;

// ── Wait helpers ──────────────────────────────────────────────────────────

export async function waitForCardCapture(
  proposalId: string,
  timeoutMs: number = 120_000,
): Promise<CapturedCardPayload> {
  const { captureTransport } = await import("./capture-transport.ts");
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const card = captureTransport.cards.get(proposalId);
    if (card) {
      console.log(`  Card captured at t=${Date.now() - (deadline - timeoutMs)}ms`);
      return card;
    }
    await sleep(1_000);
  }
  throw new Error(`card not captured for ${proposalId} within ${timeoutMs}ms`);
}

export async function waitForAnyProposal(
  timeoutMs: number = 180_000,
  afterSequence?: number,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const proposalEvent = await waitForProposal(120_000, afterSequence);
      if (proposalEvent) return proposalEvent;
    } catch {}
    await sleep(2_000);
  }
  throw new Error(`no proposal found within ${timeoutMs}ms`);
}

export async function waitForProposalReady(
  proposalId: string,
  timeoutMs: number = 120_000,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest(
      "GET", `/v1/capability-change-proposals/${proposalId}`, null, DECISION_TOKEN,
    );
    if (resp.ok && resp.data?.approval?.status === "PendingApproval") {
      return resp.data;
    }
    await sleep(2_000);
  }
  throw new Error(`proposal ${proposalId} not PendingApproval within ${timeoutMs}ms`);
}

export async function waitForComponent(
  componentId: string,
  timeoutMs: number = 120_000,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest(
      "GET", `/v1/components/${componentId}`, null, DECISION_TOKEN,
    );
    if (resp.ok && resp.data?.component?.component_id) {
      console.log(`  Component ${componentId} found at t=${Date.now() - (deadline - timeoutMs)}ms`);
      return resp.data;
    }
    await sleep(2_000);
  }
  throw new Error(`component ${componentId} not found within ${timeoutMs}ms`);
}

export async function waitForAnyComponent(
  timeoutMs: number = 120_000,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest("GET", "/v1/components", null, DECISION_TOKEN);
    if (resp.ok && resp.data?.components?.length > 0) {
      return resp.data.components;
    }
    await sleep(2_000);
  }
  throw new Error(`no components found within ${timeoutMs}ms`);
}

export async function disableComponent(
  componentId: string,
  expectedComponentSnapshotId: string,
  expectedDeploymentId: string,
): Promise<any> {
  const body = {
    component_snapshot_id: expectedComponentSnapshotId,
    deployment_record_id: expectedDeploymentId,
  };
  return kernelRequest("POST", `/v1/components/${componentId}/disable`, body, DECISION_TOKEN);
}

export async function injectShadowMarker(
  runId: string,
  suffix?: string,
): Promise<any> {
  const markerId = `shadow_marker_${runId}${suffix ? `_${suffix}` : ""}`;
  const body = { content: markerId };
  return kernelRequest("POST", "/v1/event", body, DECISION_TOKEN);
}

export async function getCurrentCursor(): Promise<number> {
  const resp = await kernelRequest("GET", "/v1/events/cursor", null, DECISION_TOKEN);
  if (resp.ok && typeof resp.data?.cursor === "number") return resp.data.cursor;
  return 0;
}

export async function waitForMarkerIngested(
  markerId: string,
  timeoutMs: number = 120_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest("GET", `/v1/events/observe?limit=100`, null, DECISION_TOKEN);
    if (resp.ok && resp.data?.events) {
      for (const event of resp.data.events) {
        if (event.correlation_id === markerId) return;
      }
    }
    await sleep(2_000);
  }
  throw new Error(`marker ${markerId} not ingested within ${timeoutMs}ms`);
}

export async function loopbackRequest(
  targetComponent: string,
  operation: string,
  args: any,
): Promise<any> {
  const body = {
    protocol_version: "process-harness-v1",
    operation,
    component_id: targetComponent,
    arguments: args,
  };
  return kernelRequest("POST", "/v1/invoke", body, IPC_TOKEN);
}

export async function waitForComponentCursor(
  componentId: string,
  expectedVersion: string,
  timeoutMs: number = 120_000,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest(
      "GET", `/v1/components/${componentId}`, null, DECISION_TOKEN,
    );
    if (resp.ok && resp.data?.component?.version === expectedVersion) {
      return resp.data;
    }
    await sleep(2_000);
  }
  throw new Error(`component ${componentId} version ${expectedVersion} not found within ${timeoutMs}ms`);
}

// ── Development cycle result ──────────────────────────────────────────────

export interface DevelopmentCycleResult {
  phase: string;
  messageId: string;
  externalEventId: string;
  proposalId: string;
  approvalId: string;
  decisionNonce: string;
  phaseStartCursor: number;
  componentId?: string;
  componentSnapshotId?: string;
  version?: string;
  deploymentId?: string;
  artifactDigest?: string;
  manifestDigest?: string;
  manifestRef?: string;
  activatedManifestDigest?: string;
  failedReceiptId?: string;
}

// ── Core development cycle ────────────────────────────────────────────────

/**
 * Run one complete development cycle:
 *   message → proposal → card → approval → deployment/activation
 *
 * @param expectDeployment - "success" or "failure"
 */
export async function runDevelopmentCycle(
  phase: string,
  messageText: string,
  messageId: string,
  senderOpenId: string,
  phaseStartCursor: number,
  expectedComponentId: string,
  expectDeployment: "success" | "failure",
): Promise<DevelopmentCycleResult> {
  const result: DevelopmentCycleResult = {
    phase, messageId, externalEventId: messageId,
    proposalId: "", approvalId: "", decisionNonce: "",
    phaseStartCursor,
  };

  // Step 1: Simulate Feishu message
  console.log(`\n[${phase}-1] Simulating Feishu message...`);
  const msgResult = await simulateFeishuMessage(messageText, messageId, senderOpenId);
  if (!msgResult.ok) {
    evidence.fail(`PHASE_${phase}_INGRESS`, `simulateFeishuMessage failed: ${JSON.stringify(msgResult)}`, msgResult);
    return result;
  }
  evidence.pass(`PHASE_${phase}_INGRESS`, `message ${messageId} sent`, msgResult);

  // Step 2: Wait for proposal
  console.log(`\n[${phase}-2] Waiting for proposal creation...`);
  try {
    const proposalEvent = await waitForAnyProposal(300_000, phaseStartCursor);
    result.proposalId = proposalEvent.proposal_id;
    result.approvalId = proposalEvent.approval_id || "";
    result.decisionNonce = proposalEvent.decision_nonce || "";
    console.log(`  Proposal created: ${result.proposalId}`);
  } catch (err: any) {
    evidence.fail(`PHASE_${phase}_PROPOSAL`, `no proposal found: ${err.message}`, { error: err.message });
    return result;
  }

  // Wait for PendingApproval
  let proposalData: any;
  try {
    proposalData = await waitForProposalReady(result.proposalId, 300_000);
  } catch (err: any) {
    evidence.fail(`PHASE_${phase}_PROPOSAL_READY`, `proposal ${result.proposalId} not PendingApproval: ${err.message}`);
    return result;
  }
  result.artifactDigest = proposalData.artifact_digest;
  result.manifestDigest = proposalData.manifest_digest;
  result.manifestRef = proposalData.manifest_ref;
  evidence.pass(`PHASE_${phase}_PROPOSAL_READY`, `proposal ${result.proposalId} is PendingApproval`, {
    approval_id: result.approvalId,
    artifact_digest: result.artifactDigest,
    manifest_digest: result.manifestDigest,
    manifest_ref: result.manifestRef,
  });

  // Step 3: Wait for card delivery
  console.log(`\n[${phase}-3] Waiting for card delivery...`);
  let cardPayload: CapturedCardPayload;
  try {
    cardPayload = await waitForCardCapture(result.proposalId, 120_000);
  } catch (err: any) {
    evidence.fail(`PHASE_${phase}_CARD`, `card not captured: ${err.message}`);
    return result;
  }
  if (!cardPayload.bindings) {
    evidence.fail(`PHASE_${phase}_CARD`, "captured card has no bindings", cardPayload);
    return result;
  }
  if (cardPayload.bindings.proposal_id !== result.proposalId) {
    evidence.fail(`PHASE_${phase}_CARD`, `proposal_id mismatch: ${cardPayload.bindings.proposal_id}`, cardPayload);
    return result;
  }
  evidence.pass(`PHASE_${phase}_CARD`, `card captured for ${result.proposalId}`, {
    approval_id: cardPayload.bindings.approval_id,
  });

  // Step 4: Card approval callback
  console.log(`\n[${phase}-4] Simulating card approval callback...`);
  const callbackResult = await simulateCardApproval(result.proposalId);
  if (!callbackResult.ok) {
    evidence.fail(`PHASE_${phase}_CALLBACK`, `callback failed: ${callbackResult.toast}`, callbackResult);
    return result;
  }
  evidence.pass(`PHASE_${phase}_CALLBACK`, `callback approved for ${result.proposalId}`, {
    toast: callbackResult.toast,
  });

  // Step 5: Wait for deployment/activation
  console.log(`\n[${phase}-5] Waiting for ${expectDeployment === "success" ? "successful" : "failed"} deployment...`);

  if (expectDeployment === "success") {
    // Wait for the component to appear
    let componentData: any;
    try {
      componentData = await waitForComponent(expectedComponentId, 300_000);
    } catch (err: any) {
      evidence.fail(`PHASE_${phase}_DEPLOYMENT`, `component ${expectedComponentId} not found: ${err.message}`);
      return result;
    }
    result.componentId = componentData.component?.component_id;
    result.componentSnapshotId = componentData.component_snapshot_id;
    result.version = componentData.component?.version;
    result.deploymentId = componentData.component?.deployment_id;

    // Wait for the activated manifest digest
    if (componentData.component?.activated_manifest?.manifest_digest) {
      result.activatedManifestDigest = componentData.component.activated_manifest.manifest_digest;
    }

    evidence.pass(`PHASE_${phase}_DEPLOYMENT`, `component ${expectedComponentId} deployed`, {
      component_id: result.componentId,
      version: result.version,
      activated_manifest_digest: result.activatedManifestDigest,
    });
  } else {
    // Expected failure — wait for ActivationFailed
    try {
      const failedEvent = await waitForActivationFailed(result.proposalId, 180_000);
      result.failedReceiptId = failedEvent?.receipt_id || "";
      evidence.pass(`PHASE_${phase}_DEPLOYMENT_FAILED`, `activation failed as expected`, {
        receipt_id: result.failedReceiptId,
      });
    } catch (err: any) {
      evidence.fail(`PHASE_${phase}_DEPLOYMENT_FAILURE`, `expected failure not observed: ${err.message}`);
      return result;
    }
  }

  return result;
}

// ── Activation-failed waiter ──────────────────────────────────────────────

async function waitForActivationFailed(
  proposalId: string,
  timeoutMs: number = 180_000,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest(
      "GET", `/v1/capability-change-proposals/${proposalId}`, null, DECISION_TOKEN,
    );
    if (resp.ok && resp.data?.status === "ActivationFailed") {
      return resp.data;
    }
    await sleep(3_000);
  }
  throw new Error(`proposal ${proposalId} not ActivationFailed within ${timeoutMs}ms`);
}
