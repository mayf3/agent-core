//! Shared development cycle — runs the full flow from message to deployment/activation.

import { kernelRequest, sleep } from "./clients/http-client.ts";
import { evidence } from "./evidence.ts";
import {
  simulateFeishuMessage,
  simulateCardApproval,
  config,
  transport as shadowTransport,
} from "./connector-shadow.ts";
import type { CapturedCardPayload } from "./capture-transport.ts";

const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const IPC_TOKEN = process.env.AGENT_CORE_IPC_TOKEN || "";
const OBSERVE_TOKEN = process.env.AGENT_CORE_EVENT_OBSERVE_TOKEN || "";
const KERNEL_PORT = parseInt(process.env.AGENT_CORE_KERNEL_PORT || "4130", 10);
const KERNEL_BASE = `http://127.0.0.1:${KERNEL_PORT}`;

// ── Wait helpers ──────────────────────────────────────────────────────────

export async function waitForCardCapture(
  proposalId: string,
  timeoutMs: number = 120_000,
): Promise<CapturedCardPayload> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const card = shadowTransport.findCardByProposalId(proposalId);
    if (card) {
      console.log(`  Card captured at t=${Date.now() - (deadline - timeoutMs)}ms`);
      return card;
    }
    await sleep(1_000);
  }
  throw new Error(`card not captured for ${proposalId} within ${timeoutMs}ms`);
}

/**
 * Wait for any CapabilityChangeProposed event to appear in the journal.
 *
 * Polls the event observe endpoint for CapabilityChangeProposed events
 * after the given cursor sequence. Returns the event payload which
 * contains proposal_id and (for HCR-derived proposals) hcr_id.
 *
 * This replaces the previous implementation that incorrectly passed a
 * numeric literal as a proposal ID to waitForProposal().
 */
export async function waitForAnyProposal(
  timeoutMs: number = 180_000,
  afterSequence?: number,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  const cursor = typeof afterSequence === "number" ? afterSequence : 0;

  while (Date.now() < deadline) {
    try {
      const resp = await kernelRequest(
        "GET",
        `/v1/events?event_kind=CapabilityChangeProposed&cursor=${cursor}&limit=10`,
        null,
        OBSERVE_TOKEN,
      );
      if (resp.ok && resp.data?.events?.length > 0) {
        const event = resp.data.events[0];
        const payload = event.payload || {};
        console.log(`  Found CapabilityChangeProposed event: proposal_id=${payload.proposal_id} event_id=${event.event_id}`);
        return {
          proposal_id: payload.proposal_id,
          hcr_id: payload.hcr_id,
          candidate_ref: payload.candidate_ref || (payload.candidate_id ? `generated/${payload.candidate_id}/candidate` : ""),
          claim_id: payload.claim_id || "",
          run_id: event.run_id || payload.run_id || "",
          principal_id: event.principal_id || payload.submitter || "",
          session_id: event.session_id || payload.session_id || "",
          registry_snapshot_id: payload.expected_snapshot_id || payload.registry_snapshot_id || "",
          ...payload,
        };
      }
    } catch (err: any) {
      console.log(`  waitForAnyProposal poll error: ${err.message}`);
    }
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
    if (resp.ok && (resp.data?.approval?.status === "Pending" || resp.data?.status === "PendingApproval")) {
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

// ── Calculator invoke helpers ─────────────────────────────────────────────

/**
 * Find the run_id spawned to process a given ingress kernelEventId.
 */
async function findRunForIngress(
  kernelEventId: string,
  timeoutMs: number = 30_000,
): Promise<string | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest("GET", `/v1/events?limit=200`, null, OBSERVE_TOKEN);
    if (!resp.ok || !resp.data?.events) {
      await sleep(1_000);
      continue;
    }
    for (const evt of resp.data.events) {
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
    const resp = await kernelRequest("GET", `/v1/events?limit=100`, null, OBSERVE_TOKEN);
    if (!resp.ok || !resp.data?.events) {
      await sleep(1_000);
      continue;
    }
    for (const evt of resp.data.events) {
      if (
        evt.event_kind === "AssistantReplyDelivered" &&
        evt.run_id === invokeRunId
      ) {
        const rawText = evt.payload?.text;
        if (rawText === undefined || rawText === null) return null;
        const parsed = typeof rawText === "number" ? rawText : Number(rawText);
        if (!Number.isFinite(parsed)) return null;
        return parsed;
      }
    }
    await sleep(1_000);
  }
  return null;
}

/**
 * Invoke the north-star calculator via the smoke sentence ingress and
 * return the real numeric result. The calculator_router matches the
 * fixed sentence and triggers calculator_delivery → capability host.
 *
 * @returns {Promise<{ok:boolean, result?:number, runId?:string, error?:string}>}
 */
export async function invokeCalculator(
  messageId: string,
  senderOpenId: string,
): Promise<{ ok: boolean; result?: number; runId?: string; error?: string }> {
  const invokeResult = await simulateFeishuMessage(
    "用 external.calculator 计算 6 * 7",
    messageId,
    senderOpenId,
  );
  if (!invokeResult.ok) {
    return { ok: false, error: `smoke sentence ingress failed: ${invokeResult.status}` };
  }
  const invokeKernelEventId = invokeResult.kernelEventId || "";
  const runId = await findRunForIngress(invokeKernelEventId);
  if (!runId) {
    return { ok: false, error: `could not find RunStarted for ingress ${invokeKernelEventId}` };
  }
  const rawResult = await waitForCalculatorResult(runId);
  if (rawResult === null) {
    return { ok: false, error: `calculator did not return a numeric result for run ${runId} within timeout` };
  }
  return { ok: true, result: rawResult, runId };
}

export async function getCurrentCursor(): Promise<number> {
  // /v1/events/cursor does not exist. Query the max sequence from the
  // events table by requesting 1 event at a known-high cursor and
  // reading next_cursor.  The events API returns next_cursor = last_seq+1
  // so the returned value is a safe cursor for the next poll.
  const resp = await kernelRequest("GET", "/v1/events?limit=1000", null, OBSERVE_TOKEN);
  if (resp.ok && typeof resp.data?.next_cursor === "number") return resp.data.next_cursor;
  if (resp.ok && resp.data?.events?.length > 0) {
    // Fallback: estimate cursor from event count
    return resp.data.events.length + 1;
  }
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
    // Wait for the component to appear (service components) or
    // for the proposal to reach Activated state (invocable capabilities).
    let componentData: any;
    try {
      componentData = await waitForComponent(expectedComponentId, 60_000);
    } catch {
      // Component not found within 60s — may be an invocable capability
      // (e.g. external.calculator) that doesn't register a component entry.
      // Fall back to checking proposal status.
      const activated = await waitForActivated(result.proposalId, 300_000);
      componentData = activated ? { component: { component_id: expectedComponentId } } : null;
      if (!componentData) {
        evidence.fail(`PHASE_${phase}_DEPLOYMENT`, `proposal ${result.proposalId} not Activated`);
        return result;
      }
      console.log(`  Proposal ${result.proposalId} Activated (invocable capability)`);
      result.activatedManifestDigest = activated?.activated_manifest_digest || result.manifestDigest || "";
    }
    if (componentData) {
      result.componentId = componentData.component?.component_id || expectedComponentId;
      result.componentSnapshotId = componentData.component_snapshot_id || "";
      result.version = componentData.component?.version || "";
      result.deploymentId = componentData.component?.deployment_id || "";
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

// ── Activation waiters ────────────────────────────────────────────────────

async function waitForActivated(
  proposalId: string,
  timeoutMs: number = 180_000,
): Promise<any> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await kernelRequest(
      "GET", `/v1/capability-change-proposals/${proposalId}`, null, DECISION_TOKEN,
    );
    if (resp.ok && resp.data?.status === "Activated") {
      return resp.data;
    }
    await sleep(2_000);
  }
  return null;
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
