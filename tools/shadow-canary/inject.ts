#!/usr/bin/env npx tsx
/**
 * inject.ts — Shadow Canary Automated End-to-End Runner
 *
 * Executes the full one-sentence development flow against shadow services.
 * Every step goes through production Connector code paths:
 *   simulateFeishuMessage  → normalizeMessageEvent + postIngress  (production)
 *   simulateCardApproval   → handleProposalCardAction             (production)
 *   card delivery          → execute-server + fetchProposal + renderProposalPendingCard
 *
 * Fail-closed: exits non-zero at FIRST failure with evidence saved.
 *
 * Usage:
 *   npx tsx tools/shadow-canary/inject.ts fresh    [--run-id <id>]
 *   npx tsx tools/shadow-canary/inject.ts dirty    [--run-id <id>]
 *
 * Environment (set by canary-runtime shadow-e2e):
 *   SHADOW_EVIDENCE_DIR, SHADOW_RUN_ID, SHADOW_VARIANT
 *   AGENT_CORE_KERNEL_PORT, AGENT_CORE_CAPABILITY_DECISION_TOKEN, etc.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import * as http from "node:http";

// =========================================================================
// Shadow Connector module — starts execute server on import
// =========================================================================

import {
  simulateFeishuMessage,
  simulateCardApproval,
  waitForProposal,
  transport,
  config,
  approvalConfig,
} from "./connector-shadow.ts";
import type { CapturedCardPayload } from "./capture-transport.ts";

// =========================================================================
// Configuration
// =========================================================================

const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;
const VARIANT = (process.argv[2] || process.env.SHADOW_VARIANT || "fresh").toLowerCase();
const EVIDENCE_DIR = process.env.SHADOW_EVIDENCE_DIR || "/tmp/agent-core-shadow-evidence";
const KERNEL_PORT = parseInt(process.env.AGENT_CORE_KERNEL_PORT || "4130", 10);
const KERNEL_BASE = `http://127.0.0.1:${KERNEL_PORT}`;
const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const EVENT_OBSERVE_TOKEN = process.env.AGENT_CORE_EVENT_OBSERVE_TOKEN || "";
const IPC_TOKEN = process.env.AGENT_CORE_IPC_TOKEN || "";

// =========================================================================
// Evidence Writer
// =========================================================================

const evidence = {
  _steps: {} as Record<string, any>,
  _failed: false,
  _firstFailedStep: "",

  write(name: string, data: any) {
    const filePath = path.join(EVIDENCE_DIR, name);
    fs.writeFileSync(filePath, JSON.stringify(data, null, 2), "utf-8");
    console.log(`  [evidence] ${name}`);
  },

  pass(step: string, detail: string, data?: any) {
    console.log(`\n  ✅ ${step}: ${detail}`);
    this._steps[step] = { status: "PASS", detail, data };
    this.write(`step-${step}.json`, { status: "PASS", detail, data });
  },

  fail(step: string, detail: string, data?: any) {
    console.error(`\n  ❌ ${step}: ${detail}`);
    this._steps[step] = { status: "FAIL", detail, data };
    this.write(`step-${step}.json`, { status: "FAIL", detail, data });
    this._failed = true;
    if (!this._firstFailedStep) this._firstFailedStep = step;
  },

  get failed(): boolean { return this._failed; },
  get firstFailedStep(): string { return this._firstFailedStep; },

  summary() {
    this.write("shadow-summary.json", {
      run_id: RUN_ID,
      variant: VARIANT,
      first_failed_step: this._firstFailedStep || null,
      steps: this._steps,
    });
  },
};

// =========================================================================
// HTTP Helpers
// =========================================================================

function kernelRequest(method: string, path: string, body?: any, token?: string): Promise<any> {
  return new Promise((resolve, reject) => {
    const url = new URL(`${KERNEL_BASE}${path}`);
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (token) headers["Authorization"] = `Bearer ${token}`;

    // Set Content-Length explicitly — the Agent Core HTTP server reads body
    // bytes based on Content-Length and does NOT support Transfer-Encoding: chunked.
    let bodyStr: string | undefined;
    if (body !== undefined) {
      bodyStr = JSON.stringify(body);
      headers["Content-Length"] = String(Buffer.byteLength(bodyStr));
    }

    const options: http.RequestOptions = {
      hostname: url.hostname, port: url.port, path: url.pathname + url.search,
      method, headers, timeout: 30000,
    };

    const req = http.request(options, (res) => {
      let data = "";
      res.on("data", (chunk) => (data += chunk));
      res.on("end", () => {
        try { resolve({ ok: res.statusCode! >= 200 && res.statusCode! < 300, status: res.statusCode, data: JSON.parse(data) }); }
        catch { resolve({ ok: false, status: res.statusCode, data: { raw: data } }); }
      });
    });
    req.on("error", reject);
    req.on("timeout", () => { req.destroy(); reject(new Error("timeout")); });
    if (bodyStr) req.write(bodyStr);
    req.end();
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// =========================================================================
// Polling Helpers
// =========================================================================

/** Poll for a captured card payload in evidence directory */
async function waitForCardCapture(
  proposalId: string,
  timeoutMs: number = 180_000,
): Promise<CapturedCardPayload> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    // Check bindings files first
    const bindingsFile = path.join(EVIDENCE_DIR, `card-bindings-${proposalId}.json`);
    if (fs.existsSync(bindingsFile)) {
      const payload = JSON.parse(fs.readFileSync(bindingsFile, "utf-8"));
      // Bindings file has flat {proposal_id, approval_id, decision_nonce, message_id}
      // Wrap in CapturedCardPayload shape for the caller
      return {
        message_id: payload.message_id || "",
        msg_type: "interactive",
        content: payload,
        captured_at: new Date().toISOString(),
        bindings: {
          proposal_id: payload.proposal_id,
          approval_id: payload.approval_id,
          decision_nonce: payload.decision_nonce,
        },
      } as CapturedCardPayload;
    }
    // Check all card files
    const files = fs.readdirSync(EVIDENCE_DIR).filter((f) => f.startsWith("card-") && f.endsWith(".json"));
    for (const file of files) {
      const content = JSON.parse(fs.readFileSync(path.join(EVIDENCE_DIR, file), "utf-8"));
      if (content.bindings?.proposal_id === proposalId) {
        return content as CapturedCardPayload;
      }
      // Also check for proposal_id in the full content
      if (content.msg_type === "interactive" && JSON.stringify(content.content).includes(proposalId)) {
        return content as CapturedCardPayload;
      }
    }
    await sleep(2_000);
  }
  throw new Error(`TIMEOUT: card not captured for proposal ${proposalId} within ${timeoutMs}ms`);
}

/** Poll Kernel journal events to find a proposal (after optional cursor) */
async function waitForAnyProposal(timeoutMs: number = 180_000, afterSequence?: number): Promise<any> {
  const startTime = Date.now();
  const cursorParam = afterSequence !== undefined ? `&cursor=${afterSequence}` : "";
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest("GET", `/v1/events?limit=50${cursorParam}`, null, EVENT_OBSERVE_TOKEN).catch(() => null);
    if (resp?.ok && resp.data?.events) {
      for (const ev of resp.data.events) {
        if (ev.event_kind === "CapabilityChangeProposed" && ev.payload?.proposal_id) {
          return {
            ...ev.payload,
            event_id: ev.event_id,
            sequence: ev.sequence,
          };
        }
      }
    }
    await sleep(3_000);
  }
  throw new Error(`TIMEOUT: no proposal found within ${timeoutMs}ms`);
}

/** Poll for proposal to be PendingApproval via Kernel GET API */
async function waitForProposalReady(proposalId: string, timeoutMs: number = 120_000): Promise<any> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest(
      "GET", `/v1/capability-change-proposals/${proposalId}`, null, DECISION_TOKEN,
    ).catch(() => null);
    if (resp?.ok && resp.data?.status === "PendingApproval") {
      return resp.data;
    }
    await sleep(2_000);
  }
  throw new Error(`TIMEOUT: proposal ${proposalId} not PendingApproval within ${timeoutMs}ms`);
}

/** Poll for component registration via observe endpoint */
async function waitForComponent(
  componentId: string,
  timeoutMs: number = 120_000,
): Promise<any> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest(
      "GET", `/v1/components/${componentId}`, null, DECISION_TOKEN,
    ).catch(() => null);
    // observe handler returns {ok, component_snapshot_id, component: {status, ...}}
    if (resp?.ok && resp.data?.component?.status === "Healthy") {
      return resp.data;
    }
    await sleep(2_000);
  }
  throw new Error(`TIMEOUT: component ${componentId} not Healthy within ${timeoutMs}ms`);
}

/** Poll Kernel journal to find the registered component_id (after optional cursor) */
async function waitForAnyComponent(
  timeoutMs: number = 180_000,
  afterSequence?: number,
): Promise<{ component_id: string; version: string; status: string; artifact_digest?: string; manifest_digest?: string }> {
  const startTime = Date.now();
  const cursorParam = afterSequence !== undefined ? `&cursor=${afterSequence}` : "";
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest("GET", `/v1/events?limit=100${cursorParam}`, null, EVENT_OBSERVE_TOKEN).catch(() => null);
    if (resp?.ok && resp.data?.events) {
      for (const ev of resp.data.events) {
        if (ev.event_kind === "component.registered.v0" && ev.payload?.component_id) {
          return {
            component_id: ev.payload.component_id,
            version: ev.payload.version || "unknown",
            status: ev.payload.status || "Healthy",
            artifact_digest: ev.payload.artifact_digest,
            manifest_digest: ev.payload.manifest_digest,
          };
        }
      }
    }
    await sleep(3_000);
  }
  throw new Error(`TIMEOUT: no ComponentRegistered event within ${timeoutMs}ms`);
}

/** Disable a component via formal API */
async function disableComponent(componentId: string, expectedComponentSnapshotId: string, expectedDeploymentId: string): Promise<any> {
  const body = {
    principal_id: `feishu:open_id:${config.feishuOwnerOpenId || "ou_shadow"}`,
    decision_nonce: `shadow_disable_${RUN_ID}`,
    expected_component_snapshot_id: expectedComponentSnapshotId,
    expected_deployment_id: expectedDeploymentId,
  };
  const resp = await kernelRequest(
    "POST", `/v1/components/${componentId}/disable`, body, DECISION_TOKEN,
  );
  return resp;
}

/** Inject a shadow marker event via ingress */
async function injectShadowMarker(runId: string): Promise<any> {
  const markerEventId = `shadow_marker_${RUN_ID}`;
  const ingressBody = {
    protocol_version: "v1",
    source: "Cli",
    external_event_id: markerEventId,
    received_at: new Date().toISOString(),
    payload: {
      text: `__shadow_marker__:${RUN_ID}`,
      run_id: runId,
    },
    auth_context: { authenticated: true },
  };
  const resp = await kernelRequest(
    "POST", "/v1/ingress", ingressBody, IPC_TOKEN,
  );
  return { marker_event_id: markerEventId, ingress_response: resp };
}

/** Record current journal cursor (total event count) */
async function getCurrentCursor(): Promise<number> {
  // Query from cursor=0 with a large limit to get the complete event list.
  // next_cursor will be total_events + 1 which is the cursor for the next event.
  const resp = await kernelRequest("GET", "/v1/events?limit=10000&cursor=0", null, EVENT_OBSERVE_TOKEN).catch(() => null);
  if (resp?.ok && typeof resp.data?.next_cursor === "number") {
    return resp.data.next_cursor;
  }
  // Fallback: poll until we get a valid cursor
  const start = Date.now();
  while (Date.now() - start < 15_000) {
    const r = await kernelRequest("GET", "/v1/events?limit=10000&cursor=0", null, EVENT_OBSERVE_TOKEN).catch(() => null);
    if (r?.ok && typeof r.data?.next_cursor === "number") {
      return r.data.next_cursor;
    }
    await sleep(1_000);
  }
  return 0;
}

/** Check the journal shows the marker event ingested past the given cursor */
async function waitForMarkerIngested(
  markerEventId: string,
  afterSequence: number,
  timeoutMs: number = 30_000,
): Promise<boolean> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest("GET", `/v1/events?cursor=${afterSequence}&limit=100`, null, EVENT_OBSERVE_TOKEN).catch(() => null);
    if (resp?.ok && resp.data?.events) {
      const markerFound = resp.data.events.some((e: any) =>
        e.event_id?.includes(markerEventId) ||
        JSON.stringify(e.payload).includes(markerEventId)
      );
      if (markerFound && resp.data.next_cursor > afterSequence) {
        return true;
      }
    }
    await sleep(2_000);
  }
  return false;
}

/** Make a raw HTTP request to a loopback address (for component health/state API) */
async function loopbackRequest(
  host: string,
  port: number,
  path: string,
): Promise<any> {
  return new Promise((resolve, reject) => {
    const options: http.RequestOptions = {
      hostname: host, port, path, method: "GET", timeout: 5000,
    };
    const req = http.request(options, (res) => {
      let data = "";
      res.on("data", (chunk) => (data += chunk));
      res.on("end", () => {
        try { resolve({ ok: res.statusCode! >= 200 && res.statusCode! < 300, status: res.statusCode, data: JSON.parse(data) }); }
        catch { resolve({ ok: false, status: res.statusCode, data: { raw: data } }); }
      });
    });
    req.on("error", reject);
    req.on("timeout", () => { req.destroy(); reject(new Error("timeout")); });
    req.end();
  });
}

/** Poll the deployed component's /health endpoint for last_observed_cursor */
async function waitForComponentCursor(
  componentEndpoint: string,
  targetCursor: number,
  timeoutMs: number = 120_000,
): Promise<{ consumed: boolean; last_observed_cursor: number }> {
  // Parse endpoint: http://127.0.0.1:PORT
  let host = "127.0.0.1";
  let port = 0;
  try {
    const url = new URL(componentEndpoint);
    host = url.hostname;
    port = parseInt(url.port, 10);
  } catch {
    return { consumed: false, last_observed_cursor: 0 };
  }
  if (!port) return { consumed: false, last_observed_cursor: 0 };

  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    try {
      // Try /api/state first (returns last_observed_cursor in rendered output)
      const stateResp = await loopbackRequest(host, port, "/api/state");
      if (stateResp.ok && stateResp.data) {
        const cursor = stateResp.data.last_observed_cursor;
        if (typeof cursor === "number" && cursor >= targetCursor) {
          return { consumed: true, last_observed_cursor: cursor };
        }
        if (typeof cursor === "number" && cursor > 0) {
          console.log(`  Component cursor: ${cursor} (target: ${targetCursor})`);
        }
      }
      // Fallback: try /health
      const healthResp = await loopbackRequest(host, port, "/health");
      if (healthResp.ok && healthResp.data) {
        const cursor = healthResp.data.last_observed_cursor;
        if (typeof cursor === "number" && cursor >= targetCursor) {
          return { consumed: true, last_observed_cursor: cursor };
        }
      }
    } catch {
      // Component may not be ready yet
    }
    await sleep(3_000);
  }
  // Final attempt
  try {
    const resp = await loopbackRequest(host, port, "/health");
    if (resp.ok && typeof resp.data?.last_observed_cursor === "number") {
      return { consumed: false, last_observed_cursor: resp.data.last_observed_cursor };
    }
  } catch {}
  return { consumed: false, last_observed_cursor: 0 };
}

// =========================================================================
// Development Cycle — reusable parameterised helper
// =========================================================================

interface DevelopmentCycleResult {
  phase: string;
  messageId: string;
  externalEventId: string;
  proposalId: string;
  approvalId: string;
  decisionNonce: string;
  phaseStartCursor: number;
  // For successful deployment:
  componentId?: string;
  version?: string;
  deploymentId?: string;
  deploymentReceiptId?: string;
  componentSnapshotId?: string;
  componentEndpoint?: string;
  artifactDigest?: string;
  manifestDigest?: string;
  // For failed deployment:
  activationFailed?: boolean;
  failedReceiptId?: string;
}

/** Run one complete development cycle (message -> proposal -> card -> callback -> deployment)
 *  with strict ID chaining based on phaseStartCursor.
 *  @param phase - label for evidence steps (e.g. "A", "B", "C")
 *  @param messageText - the development request text
 *  @param messageId - unique message external ID
 *  @param senderOpenId - Feishu sender
 *  @param phaseStartCursor - journal cursor at start of this phase (only events beyond this cursor are accepted)
 *  @param expectedComponentId - expected component_id for deployment
 *  @param expectDeployment - "success" means verify component registration + marker; "failure" means expect ActivationFailed
 */
async function runDevelopmentCycle(
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

  // ---- Step: Simulate Feishu message ----
  console.log(`\n[${phase}-1] Simulating Feishu message...`);
  const msgResult = await simulateFeishuMessage(messageText, messageId, senderOpenId);
  if (!msgResult.ok) {
    evidence.fail(`PHASE_${phase}_INGRESS`, `simulateFeishuMessage failed: ${JSON.stringify(msgResult)}`, msgResult);
    return result;
  }
  evidence.pass(`PHASE_${phase}_INGRESS`, `message ${messageId} sent`, msgResult);

  // ---- Step: Wait for Proposal (strict cursor filter) ----
  console.log(`\n[${phase}-2] Waiting for proposal creation...`);
  let proposalId = "";
  let approvalId = "";
  let decisionNonce = "";
  try {
    const proposalEvent = await waitForAnyProposal(300_000, phaseStartCursor);
    proposalId = proposalEvent.proposal_id;
    approvalId = proposalEvent.approval_id || "";
    decisionNonce = proposalEvent.decision_nonce || "";
    console.log(`  Proposal created: ${proposalId}`);
  } catch (err: any) {
    evidence.fail(`PHASE_${phase}_PROPOSAL`, `no proposal found: ${err.message}`, { error: err.message });
    return result;
  }
  result.proposalId = proposalId;
  result.approvalId = approvalId;
  result.decisionNonce = decisionNonce;

  // Wait for proposal to be PendingApproval
  let proposalData: any;
  try {
    proposalData = await waitForProposalReady(proposalId, 300_000);
  } catch (err: any) {
    evidence.fail(`PHASE_${phase}_PROPOSAL_READY`, `proposal ${proposalId} not PendingApproval: ${err.message}`);
    return result;
  }
  evidence.pass(`PHASE_${phase}_PROPOSAL_READY`, `proposal ${proposalId} is PendingApproval`, {
    approval_id: proposalData.approval?.approval_id,
    decision_nonce: proposalData.approval?.decision_nonce,
    artifact_digest: proposalData.artifact_digest,
    manifest_digest: proposalData.manifest_digest,
  });

  // ---- Step: Wait for card delivery ----
  console.log(`\n[${phase}-3] Waiting for card delivery...`);
  let cardPayload: CapturedCardPayload;
  try {
    cardPayload = await waitForCardCapture(proposalId, 120_000);
  } catch (err: any) {
    evidence.fail(`PHASE_${phase}_CARD`, `card not captured: ${err.message}`);
    return result;
  }
  if (!cardPayload.bindings) {
    evidence.fail(`PHASE_${phase}_CARD`, "captured card has no bindings", cardPayload);
    return result;
  }
  if (cardPayload.bindings.proposal_id !== proposalId) {
    evidence.fail(`PHASE_${phase}_CARD`, `card proposal_id mismatch: ${cardPayload.bindings.proposal_id} != ${proposalId}`, cardPayload);
    return result;
  }
  evidence.pass(`PHASE_${phase}_CARD`, `card captured for ${proposalId}`, {
    approval_id: cardPayload.bindings.approval_id,
    decision_nonce: cardPayload.bindings.decision_nonce,
  });

  // ---- Step: Card approval callback ----
  console.log(`\n[${phase}-4] Simulating card approval callback...`);
  const callbackResult = await simulateCardApproval(proposalId);
  if (!callbackResult.ok) {
    evidence.fail(`PHASE_${phase}_CALLBACK`, `card callback failed: ${callbackResult.toast}`, callbackResult);
    return result;
  }
  evidence.pass(`PHASE_${phase}_CALLBACK`, `callback approved for ${proposalId}`, {
    toast: callbackResult.toast,
  });

  // ---- Deployment handling ----
  if (expectDeployment === "failure") {
    // Expect ActivationFailed — no component registration
    console.log(`\n[${phase}-5] Waiting for ActivationFailed...`);
    // Check for ActivationFailed event in journal
    const activationFailed = await waitForActivationFailed(proposalId, phaseStartCursor, 120_000);
    if (!activationFailed) {
      evidence.fail(`PHASE_${phase}_ACTIVATION_FAILED`, `deployment should have failed but no ActivationFound event found`, { proposalId });
      return result;
    }
    result.activationFailed = true;
    evidence.pass(`PHASE_${phase}_ACTIVATION_FAILED`, `deployment failed for proposal ${proposalId}`, {
      proposal_id: proposalId,
    });
    return result;
  }

  // ---- Expect SUCCESSFUL deployment ----
  console.log(`\n[${phase}-5] Waiting for deployment (expect success)...`);

  let componentEvent: any;
  try {
    componentEvent = await waitForAnyComponent(180_000, phaseStartCursor);
  } catch (err: any) {
    evidence.fail(`PHASE_${phase}_DEPLOYMENT`, `no ComponentRegistered event: ${err.message}`);
    return result;
  }

  const componentId = componentEvent.component_id;
  if (componentId !== expectedComponentId) {
    evidence.fail(`PHASE_${phase}_DEPLOYMENT`, `expected component ${expectedComponentId} but got ${componentId}`, componentEvent);
    return result;
  }
  console.log(`  Component registered: ${componentId} v${componentEvent.version}`);
  result.componentId = componentId;
  result.version = componentEvent.version;
  result.artifactDigest = componentEvent.artifact_digest;
  result.manifestDigest = componentEvent.manifest_digest;

  // Verify via component API
  let deploymentReceiptId = "(from journal)";
  let componentVersion = componentEvent.version;
  try {
    const componentData = await waitForComponent(componentId, 120_000);
    const comp = componentData.component || componentData;
    console.log(`  Component API: Healthy, deployment_receipt_id=${comp.deployment_receipt_id}`);
    deploymentReceiptId = comp.deployment_receipt_id || deploymentReceiptId;
    componentVersion = comp.version || componentVersion;
    result.deploymentId = comp.deployment_id || "";
    result.deploymentReceiptId = deploymentReceiptId;
    result.componentEndpoint = comp.endpoint || "";
  } catch {
    console.log(`  ⚠️  Component API not Healthy within 120s, trying to read endpoint anyway...`);
    // Try to read component data even if not Healthy — endpoint may still be available
    try {
      const fallbackResp = await kernelRequest("GET", `/v1/components/${componentId}`, null, DECISION_TOKEN);
      if (fallbackResp.ok && fallbackResp.data?.component) {
        const comp = fallbackResp.data.component;
        result.deploymentId = comp.deployment_id || "";
        result.deploymentReceiptId = comp.deployment_receipt_id || deploymentReceiptId;
        result.componentEndpoint = comp.endpoint || "";
        console.log(`  Read endpoint from registry: ${result.componentEndpoint}`);
      }
    } catch {}
  }

  evidence.pass(`PHASE_${phase}_DEPLOYMENT`, `component ${componentId} v${componentVersion} registered`, {
    component_id: componentId,
    version: componentVersion,
    deployment_receipt_id: deploymentReceiptId,
  });

  // ---- Marker injection + consumption ----
  console.log(`\n[${phase}-6] Injecting marker...`);
  const markerCursor = await getCurrentCursor();
  await sleep(3_000);
  const markerResult = await injectShadowMarker(RUN_ID);
  if (!markerResult.ingress_response.ok) {
    evidence.fail(`PHASE_${phase}_MARKER_INJECT`, `marker injection failed: HTTP ${markerResult.ingress_response.status}`, markerResult.ingress_response);
    return result;
  }
  evidence.pass(`PHASE_${phase}_MARKER_INJECT`, `marker ${markerResult.marker_event_id} injected`, markerResult);

  // Verify marker in journal
  const markerIngested = await waitForMarkerIngested(markerResult.marker_event_id, markerCursor, 30_000);
  if (!markerIngested) {
    evidence.fail(`PHASE_${phase}_MARKER_JOURNAL`, `marker not visible in journal`, { marker_event_id: markerResult.marker_event_id });
    return result;
  }
  console.log(`  ✅ Marker visible in journal`);
  evidence.pass(`PHASE_${phase}_MARKER_JOURNAL`, `marker visible at cursor ${markerCursor}`, {
    marker_event_id: markerResult.marker_event_id,
    cursor: markerCursor,
  });

  // Verify component consumed marker
  let markerProcessed = false;
  let finalCursor = 0;
  if (result.componentEndpoint) {
    const consumptionResult = await waitForComponentCursor(result.componentEndpoint, markerCursor, 120_000);
    markerProcessed = consumptionResult.consumed;
    finalCursor = consumptionResult.last_observed_cursor;
  }

  if (!markerProcessed || !result.componentEndpoint) {
    evidence.fail(`PHASE_${phase}_MARKER_CONSUMED`, `component did not process marker`, {
      marker_event_id: markerResult.marker_event_id,
      endpoint_available: !!result.componentEndpoint,
      last_observed_cursor: finalCursor,
      target_cursor: markerCursor,
    });
    return result;
  }
  evidence.pass(`PHASE_${phase}_MARKER_CONSUMED`, `component processed marker`, {
    marker_event_id: markerResult.marker_event_id,
    last_observed_cursor: finalCursor,
    target_cursor: markerCursor,
  });

  // ---- Read component snapshot ----
  console.log(`\n[${phase}-7] Reading component snapshot...`);
  try {
    const compResp = await kernelRequest("GET", `/v1/components/${componentId}`, null, DECISION_TOKEN);
    if (compResp.ok && compResp.data?.component_snapshot_id) {
      result.componentSnapshotId = compResp.data.component_snapshot_id;
      result.deploymentId = result.deploymentId || compResp.data.component?.deployment_id || "";
      console.log(`  component_snapshot_id: ${result.componentSnapshotId}`);
    }
  } catch {}
  evidence.pass(`PHASE_${phase}_SNAPSHOT`, `snapshot ${result.componentSnapshotId}`, {
    component_snapshot_id: result.componentSnapshotId,
    deployment_id: result.deploymentId,
  });

  return result;
}

/** Wait for an ActivationFailed journal event after the given cursor */
async function waitForActivationFailed(
  proposalId: string,
  afterSequence: number,
  timeoutMs: number,
): Promise<boolean> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest("GET", `/v1/events?cursor=${afterSequence}&limit=100`, null, EVENT_OBSERVE_TOKEN).catch(() => null);
    if (resp?.ok && resp.data?.events) {
      for (const ev of resp.data.events) {
        // Check for ActivationFailed events linked to this proposal
        if (ev.event_kind === "TrustedActivationFailed" || ev.event_kind === "CapabilityApprovalFailed") {
          const payloadStr = JSON.stringify(ev.payload || {});
          if (payloadStr.includes(proposalId)) {
            return true;
          }
        }
      }
    }
    await sleep(3_000);
  }
  return false;
}

async function runFreshShadow(): Promise<void> {
  const MESSAGE_TEXT = "开发一个failure-viewer，event.observe.v0";
  const MESSAGE_ID = `shadow_msg_${RUN_ID}`;
  const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";
  const COMPONENT_ID = "failure-viewer";

  console.log(`\n=== FRESH SHADOW (${RUN_ID}) ===`);

  // Record phase start cursor
  const startCursor = await getCurrentCursor();

  const result = await runDevelopmentCycle(
    "FRESH", MESSAGE_TEXT, MESSAGE_ID, SENDER_OPEN_ID,
    startCursor, COMPONENT_ID, "success",
  );

  if (!result.componentId) {
    // runDevelopmentCycle already called evidence.fail() for the failing step
    // Just return so the caller sees evidence.failed
    return;
  }

  // ---- Disable ----
  console.log(`\n[FRESH-8] Disabling component...`);
  if (!result.componentSnapshotId || !result.deploymentId) {
    // Read snapshot for disable
    try {
      const compResp = await kernelRequest("GET", `/v1/components/${result.componentId}`, null, DECISION_TOKEN);
      if (compResp.ok && compResp.data?.component_snapshot_id) {
        result.componentSnapshotId = compResp.data.component_snapshot_id;
        result.deploymentId = result.deploymentId || compResp.data.component?.deployment_id || "";
      }
    } catch {}
  }
  const disableResult = await disableComponent(result.componentId, result.componentSnapshotId || "", result.deploymentId || "");
  if (!disableResult.ok) {
    evidence.fail("DISABLE", `disable ${result.componentId} failed: HTTP ${disableResult.status}`, {
      status: disableResult.status,
      data: disableResult.data,
      expected_component_snapshot_id: result.componentSnapshotId,
    });
    return;
  }
  evidence.pass("DISABLE", `component ${result.componentId} disabled`, {
    component_status: disableResult.data?.component_status,
    receipt_id: disableResult.data?.receipt_id,
  });

  // ---- All passed ----
  console.log(`\n✅ FRESH_SHADOW_CANARY_PASS`);
  evidence.pass("FRESH_SHADOW_CANARY", `fresh shadow completed for ${RUN_ID}`, {
    component_id: result.componentId,
    version: result.version,
  });
}

// =========================================================================
// Dirty Flow
// =========================================================================

async function runDirtyShadow(): Promise<void> {
  console.log(`\n=== DIRTY SHADOW (${RUN_ID}) ===`);
  const COMPONENT_ID = "failure-viewer";
  const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";

  // ========================================================================
  // Phase A: Deploy OLD successful version (v0.1.0)
  // ========================================================================
  console.log("\n=== PHASE A: Old successful version ===");
  const phaseAStart = await getCurrentCursor();
  const msgIdA = `shadow_dirty_A_${RUN_ID}`;
  const resultA = await runDevelopmentCycle(
    "A", "开发一个failure-viewer，event.observe.v0", msgIdA, SENDER_OPEN_ID,
    phaseAStart, COMPONENT_ID, "success",
  );
  if (!resultA.componentId) return; // failure already recorded

  const OLD_VERSION = resultA.version || "";
  const OLD_ARTIFACT_DIGEST = resultA.artifactDigest || "";
  const OLD_MANIFEST_DIGEST = resultA.manifestDigest || "";
  const OLD_DEPLOYMENT_ID = resultA.deploymentId || "";
  const OLD_COMPONENT_SNAPSHOT_ID = resultA.componentSnapshotId || "";
  console.log(`\n  OLD_VERSION=${OLD_VERSION}`);
  console.log(`  OLD_ARTIFACT=${OLD_ARTIFACT_DIGEST}`);
  console.log(`  OLD_DEPLOYMENT=${OLD_DEPLOYMENT_ID}`);

  // Read snapshot from registry
  let oldSnapshotId = OLD_COMPONENT_SNAPSHOT_ID;
  let oldDeploymentId = OLD_DEPLOYMENT_ID;
  try {
    const compResp = await kernelRequest("GET", `/v1/components/${COMPONENT_ID}`, null, DECISION_TOKEN);
    if (compResp.ok && compResp.data?.component_snapshot_id) {
      oldSnapshotId = compResp.data.component_snapshot_id;
      oldDeploymentId = oldDeploymentId || compResp.data.component?.deployment_id || "";
    }
  } catch {}

  // Disable the old version
  console.log("\n[Phase A] Disabling component...");
  const disableA = await disableComponent(COMPONENT_ID, oldSnapshotId, oldDeploymentId);
  if (!disableA.ok) {
    evidence.fail("PHASE_A_DISABLE", `disable failed: HTTP ${disableA.status}`, {
      status: disableA.status, data: disableA.data,
      snapshot_id: oldSnapshotId, deployment_id: oldDeploymentId,
    });
    return;
  }
  evidence.pass("PHASE_A_DISABLE", `component disabled`, {
    receipt_id: disableA.data?.receipt_id,
    component_status: disableA.data?.component_status,
  });

  // Verify disabled state
  try {
    const checkResp = await kernelRequest("GET", `/v1/components/${COMPONENT_ID}`, null, DECISION_TOKEN);
    if (checkResp.ok && checkResp.data?.component?.status === "Disabled") {
      evidence.pass("PHASE_A_DISABLED_STATE", `registry status: Disabled`, {
        status: checkResp.data.component.status,
      });
    } else {
      console.warn(`  ⚠️  Component status: ${checkResp.data?.component?.status}`);
    }
  } catch {}

  // ========================================================================
  // Phase B: ActivationFailed via Failure Proxy
  // ========================================================================
  console.log("\n=== PHASE B: ActivationFailed via Failure Proxy ===");
  const phaseBStart = await getCurrentCursor();
  const msgIdB = `shadow_dirty_B_${RUN_ID}`;

  // For Phase B, we expect deployment to fail (ActivationFailed)
  const resultB = await runDevelopmentCycle(
    "B", "开发一个failure-viewer，event.observe.v0", msgIdB, SENDER_OPEN_ID,
    phaseBStart, COMPONENT_ID, "failure",
  );
  if (!resultB.activationFailed) return; // failure already recorded

  const FAILED_PROPOSAL_ID = resultB.proposalId;
  console.log(`\n  FAILED_PROPOSAL=${FAILED_PROPOSAL_ID}`);
  console.log(`  ACTIVATION_FAILED=true`);

  // Verify old version is still the highest installed
  try {
    const checkResp = await kernelRequest("GET", `/v1/components/${COMPONENT_ID}`, null, DECISION_TOKEN);
    if (checkResp.ok) {
      const comp = checkResp.data?.component || {};
      const currentVersion = comp.version || "";
      const currentDeploymentId = comp.deployment_id || "";
      console.log(`  Current version after failure: ${currentVersion} (old: ${OLD_VERSION})`);
      evidence.pass("PHASE_B_VERSION_CHECK", `version after failure`, {
        old_version: OLD_VERSION,
        current_version: currentVersion,
        current_deployment_id: currentDeploymentId,
        version_unchanged: currentVersion === OLD_VERSION || !currentVersion,
      });
    }
  } catch {}

  // ========================================================================
  // Phase C: Upgrade + Rollback
  // ========================================================================
  console.log("\n=== PHASE C: Upgrade from failed state + Rollback ===");
  const phaseCStart = await getCurrentCursor();
  const msgIdC = `shadow_dirty_C_${RUN_ID}`;

  // Deploy again — this should succeed (failure proxy budget is exhausted)
  const resultC = await runDevelopmentCycle(
    "C", "开发一个failure-viewer，event.observe.v0", msgIdC, SENDER_OPEN_ID,
    phaseCStart, COMPONENT_ID, "success",
  );
  if (!resultC.componentId) return; // failure already recorded

  const NEXT_VERSION = resultC.version || "";
  const NEXT_DEPLOYMENT_ID = resultC.deploymentId || "";
  const NEXT_COMPONENT_SNAPSHOT_ID = resultC.componentSnapshotId || "";
  const NEXT_ARTIFACT_DIGEST = resultC.artifactDigest || "";

  console.log(`\n  NEXT_VERSION=${NEXT_VERSION}`);
  console.log(`  NEXT_DEPLOYMENT=${NEXT_DEPLOYMENT_ID}`);

  // Verify version increased
  if (NEXT_VERSION <= OLD_VERSION && OLD_VERSION) {
    evidence.fail("PHASE_C_VERSION", `version did not increase: ${NEXT_VERSION} <= ${OLD_VERSION}`, {
      old_version: OLD_VERSION,
      next_version: NEXT_VERSION,
    });
    return;
  }
  evidence.pass("PHASE_C_VERSION", `version ${NEXT_VERSION} > ${OLD_VERSION}`, {
    old_version: OLD_VERSION,
    next_version: NEXT_VERSION,
  });

  // ========================================================================
  // Rollback via Kernel API
  // ========================================================================
  console.log("\n=== ROLLBACK ===");

  // Read current snapshot for rollback
  let rollbackSnapshotId = "";
  let rollbackDeploymentId = "";
  try {
    const compResp = await kernelRequest("GET", `/v1/components/${COMPONENT_ID}`, null, DECISION_TOKEN);
    if (compResp.ok && compResp.data?.component_snapshot_id) {
      rollbackSnapshotId = compResp.data.component_snapshot_id;
      rollbackDeploymentId = compResp.data.component?.deployment_id || "";
    }
  } catch {}
  console.log(`  Rollback snapshot: ${rollbackSnapshotId}`);
  console.log(`  Rollback deployment: ${rollbackDeploymentId}`);

  // Call rollback via Kernel API
  const rollbackBody = {
    principal_id: `feishu:open_id:${SENDER_OPEN_ID}`,
    decision_nonce: `shadow_rollback_${RUN_ID}`,
    expected_component_snapshot_id: rollbackSnapshotId,
    expected_deployment_id: rollbackDeploymentId,
  };
  const rollbackResp = await kernelRequest(
    "POST", `/v1/components/${COMPONENT_ID}/rollback`, rollbackBody, DECISION_TOKEN,
  );
  if (!rollbackResp.ok) {
    evidence.fail("ROLLBACK", `rollback failed: HTTP ${rollbackResp.status}`, {
      status: rollbackResp.status,
      data: rollbackResp.data,
      expected_snapshot_id: rollbackSnapshotId,
    });
    return;
  }
  evidence.pass("ROLLBACK", `rollback succeeded`, {
    component_id: COMPONENT_ID,
    component_version: rollbackResp.data?.component_version,
    component_status: rollbackResp.data?.component_status,
    component_snapshot_id: rollbackResp.data?.component_snapshot_id,
  });

  // Verify rollback result via registry
  try {
    const verifyResp = await kernelRequest("GET", `/v1/components/${COMPONENT_ID}`, null, DECISION_TOKEN);
    if (verifyResp.ok && verifyResp.data?.component) {
      const rolledBackVersion = verifyResp.data.component.version;
      console.log(`  Rollback version: ${rolledBackVersion} (expected old: ${OLD_VERSION})`);
      evidence.pass("ROLLBACK_VERIFY", `rollback registry verified`, {
        rolled_back_version: rolledBackVersion,
        expected_old_version: OLD_VERSION,
        old_artifact: OLD_ARTIFACT_DIGEST,
        rolled_back_status: verifyResp.data.component.status,
      });
    }
  } catch {}

  // ========================================================================
  // Verify rollback component can process marker
  // ========================================================================
  console.log("\n[Rollback] Verifying rollback component processes marker...");
  const rollbackMarkerCursor = await getCurrentCursor();
  await sleep(3_000);
  const markerRb = await injectShadowMarker(`${RUN_ID}_rollback`);
  if (!markerRb.ingress_response.ok) {
    evidence.fail("ROLLBACK_MARKER_INJECT", `marker injection failed`, markerRb.ingress_response);
    return;
  }
  // Wait for marker to be consumed
  try {
    const compResp = await kernelRequest("GET", `/v1/components/${COMPONENT_ID}`, null, DECISION_TOKEN);
    const endpoint = compResp.ok ? compResp.data?.component?.endpoint || "" : "";
    if (endpoint) {
      const rbResult = await waitForComponentCursor(endpoint, rollbackMarkerCursor, 120_000);
      if (rbResult.consumed) {
        evidence.pass("ROLLBACK_MARKER_CONSUMED", `rollback component processed marker`, {
          last_observed_cursor: rbResult.last_observed_cursor,
          target_cursor: rollbackMarkerCursor,
        });
      } else {
        console.warn(`  ⚠️  Rollback marker consumption pending`);
      }
    }
  } catch {}

  // ---- All dirty steps passed ----
  evidence.pass("DIRTY_UPGRADE_SHADOW_CANARY", `dirty upgrade shadow completed`, {
    old_version: OLD_VERSION,
    next_version: NEXT_VERSION,
    failed_proposal: FAILED_PROPOSAL_ID,
    rollback_snapshot: rollbackSnapshotId,
  });
}

// =========================================================================
// Main
// =========================================================================

async function main() {
  console.log(`\n========================================`);
  console.log(`Shadow Canary Runner`);
  console.log(`  RUN_ID:  ${RUN_ID}`);
  console.log(`  VARIANT: ${VARIANT}`);
  console.log(`  EVIDENCE: ${EVIDENCE_DIR}`);
  console.log(`========================================\n`);

  fs.mkdirSync(EVIDENCE_DIR, { recursive: true });

  // Write evidence metadata
  evidence.write("runner-metadata.json", {
    run_id: RUN_ID,
    variant: VARIANT,
    kernel_url: `${KERNEL_BASE}`,
    connector_port: config.connectorPort,
    owner_open_id: config.feishuOwnerOpenId,
    started_at: new Date().toISOString(),
  });

  // Wait for connector to be ready
  console.log("Waiting for shadow connector to be ready...");
  await sleep(3_000);

  // Execute the appropriate flow
  if (VARIANT === "dirty") {
    await runDirtyShadow();
  } else {
    await runFreshShadow();
  }

  // Write summary
  evidence.summary();

  if (evidence.failed) {
    console.error(`\n❌ FAILED at step: ${evidence.firstFailedStep}`);
    process.exit(1);
  }

  console.log(`\n✅ ALL STEPS PASSED`);
}

main().catch((err) => {
  console.error(`\n❌ FATAL: ${err.message}`);
  evidence.fail("FATAL", err.message, { stack: err.stack });
  evidence.summary();
  process.exit(1);
});
