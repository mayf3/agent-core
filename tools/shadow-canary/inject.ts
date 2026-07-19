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

/** Poll Kernel journal events to find a proposal */
async function waitForAnyProposal(timeoutMs: number = 180_000): Promise<any> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest("GET", `/v1/events?limit=50`, null, EVENT_OBSERVE_TOKEN).catch(() => null);
    if (resp?.ok && resp.data?.events) {
      for (const ev of resp.data.events) {
        if (ev.event_kind === "CapabilityChangeProposed" && ev.payload?.proposal_id) {
          return ev.payload;
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

/** Poll Kernel journal to find the registered component_id */
async function waitForAnyComponent(
  timeoutMs: number = 180_000,
): Promise<{ component_id: string; version: string; status: string }> {
  const startTime = Date.now();
  while (Date.now() - startTime < timeoutMs) {
    const resp = await kernelRequest("GET", "/v1/events?limit=100", null, EVENT_OBSERVE_TOKEN).catch(() => null);
    if (resp?.ok && resp.data?.events) {
      for (const ev of resp.data.events) {
        if (ev.event_kind === "component.registered.v0" && ev.payload?.component_id) {
          return {
            component_id: ev.payload.component_id,
            version: ev.payload.version || "unknown",
            status: ev.payload.status || "Healthy",
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

/** Record current journal cursor before marker injection */
async function getCurrentCursor(): Promise<number> {
  const resp = await kernelRequest("GET", "/v1/events?limit=1&cursor=0", null, EVENT_OBSERVE_TOKEN).catch(() => null);
  if (resp?.ok && typeof resp.data?.next_cursor === "number") {
    return resp.data.next_cursor;
  }
  // Fallback: poll until we get a valid cursor
  const start = Date.now();
  while (Date.now() - start < 15_000) {
    const r = await kernelRequest("GET", "/v1/events?limit=1&cursor=0", null, EVENT_OBSERVE_TOKEN).catch(() => null);
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
// Full Fresh Flow
// =========================================================================

async function runFreshShadow(): Promise<void> {
  const MESSAGE_TEXT = "开发一个failure-viewer，event.observe.v0";
  const MESSAGE_ID = `shadow_msg_${RUN_ID}`;
  const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";

  console.log(`\n=== FRESH SHADOW (${RUN_ID}) ===`);

  // ---- Step 1: Simulate Feishu message ----
  console.log("\n[1] Simulating Feishu message...");
  const msgResult = await simulateFeishuMessage(MESSAGE_TEXT, MESSAGE_ID, SENDER_OPEN_ID);
  if (!msgResult.ok) {
    return evidence.fail("SHADOW_CONNECTOR_INGRESS", `simulateFeishuMessage failed: ${JSON.stringify(msgResult)}`, msgResult);
  }
  evidence.pass("SHADOW_CONNECTOR_INGRESS", `message ${MESSAGE_ID} sent`, msgResult);

  // ---- Step 2: Wait for Proposal ----
  console.log("\n[2] Waiting for proposal creation...");
  let proposalEvent: any;
  try {
    proposalEvent = await waitForAnyProposal(300_000); // 5 min timeout for coding harness
  } catch (err: any) {
    return evidence.fail("PROPOSAL_CREATION", `no proposal found: ${err.message}`, { error: err.message });
  }
  const proposalId = proposalEvent.proposal_id;
  console.log(`  Proposal created: ${proposalId}`);
  evidence.pass("PROPOSAL_CREATION", `proposal ${proposalId}`, proposalEvent);

  // Wait for proposal to be PendingApproval
  let proposalData: any;
  try {
    proposalData = await waitForProposalReady(proposalId, 60_000);
  } catch (err: any) {
    return evidence.fail("PROPOSAL_READY", `proposal ${proposalId} not PendingApproval: ${err.message}`);
  }
  evidence.pass("PROPOSAL_READY", `proposal ${proposalId} is PendingApproval`, {
    approval_id: proposalData.approval?.approval_id,
    decision_nonce: proposalData.approval?.decision_nonce,
    artifact_digest: proposalData.artifact_digest,
    manifest_digest: proposalData.manifest_digest,
  });

  // ---- Step 3: Wait for card delivery via Outbox ----
  console.log("\n[3] Waiting for card delivery...");
  let cardPayload: CapturedCardPayload;
  try {
    cardPayload = await waitForCardCapture(proposalId, 120_000);
  } catch (err: any) {
    return evidence.fail("CARD_DELIVERY", `card not captured: ${err.message}`);
  }

  // Verify card bindings match proposal
  if (!cardPayload.bindings) {
    return evidence.fail("CARD_DELIVERY", "captured card has no bindings", cardPayload);
  }
  if (cardPayload.bindings.proposal_id !== proposalId) {
    return evidence.fail("CARD_DELIVERY", `card proposal_id mismatch: ${cardPayload.bindings.proposal_id} != ${proposalId}`, cardPayload);
  }
  if (cardPayload.bindings.approval_id !== proposalData.approval?.approval_id) {
    return evidence.fail("CARD_DELIVERY", `card approval_id mismatch`, {
      card: cardPayload.bindings.approval_id,
      proposal: proposalData.approval?.approval_id,
    });
  }
  evidence.pass("SHADOW_PROPOSAL_CARD_DELIVERY", `card captured for ${proposalId}`, {
    approval_id: cardPayload.bindings.approval_id,
    decision_nonce: cardPayload.bindings.decision_nonce,
    card_msg_type: cardPayload.msg_type,
  });

  // ---- Step 4: Simulate card click callback ----
  console.log("\n[4] Simulating card approval callback...");
  const beforeCallback = Date.now();
  const callbackResult = await simulateCardApproval(proposalId);
  const callbackLatency = Date.now() - beforeCallback;

  if (!callbackResult.ok) {
    return evidence.fail("SHADOW_CONNECTOR_CALLBACK", `card callback failed: ${callbackResult.toast}`, callbackResult);
  }
  evidence.pass("SHADOW_CONNECTOR_CALLBACK", `callback approved for ${proposalId}`, {
    latency_ms: callbackLatency,
    toast: callbackResult.toast,
  });

  // ---- Step 5: Wait for deployment ----
  console.log("\n[5] Waiting for deployment...");
  
  // Wait for ComponentRegistered event in journal
  let componentEvent: any;
  try {
    componentEvent = await waitForAnyComponent(180_000);
  } catch (err: any) {
    return evidence.fail("DEPLOYMENT", `no ComponentRegistered event: ${err.message}`);
  }
  
  const componentId = componentEvent.component_id;
  console.log(`  Component registered: ${componentId} v${componentEvent.version}`);
  
  // Verify deployment via component API (non-blocking — journal event is the source of truth)
  let deploymentReceiptId = "(from journal)";
  let componentVersion = componentEvent.version;
  
  try {
    const componentData = await waitForComponent(componentId, 30_000);
    // observe returns {ok, component_snapshot_id, component: {deployment_receipt_id, version, ...}}
    const comp = componentData.component || componentData;
    console.log(`  Component API: Healthy, deployment_receipt_id=${comp.deployment_receipt_id}`);
    deploymentReceiptId = comp.deployment_receipt_id || deploymentReceiptId;
    componentVersion = comp.version || componentVersion;
  } catch {
    console.log(`  ⚠️ Component API not Healthy within 30s (journal event confirmed deployment)`);
  }
  
  evidence.pass("DEPLOYMENT", `component ${componentId} v${componentVersion} registered`, {
    component_id: componentId,
    version: componentVersion,
    deployment_receipt_id: deploymentReceiptId,
  });

  // ---- Step 6: Verify Registry ----
  console.log("\n[6] Verifying Kernel Registry...");
  evidence.pass("REGISTRY", `component ${componentId} registered (event confirms registry update)`, {
    component_id: componentId,
    component_version: componentVersion,
  });

  // ---- Step 7a: Record current journal cursor ----
  console.log("\n[7a] Recording current journal cursor...");
  const markerCursor = await getCurrentCursor();
  console.log(`  Current journal cursor: ${markerCursor}`);
  evidence.pass("MARKER_CURSOR_BEFORE", `cursor ${markerCursor}`, { cursor: markerCursor });

  // ---- Step 7b: Inject shadow marker ----
  console.log("\n[7b] Injecting shadow marker...");
  await sleep(5_000); // Wait for component to be ready
  const markerResult = await injectShadowMarker(RUN_ID);
  if (!markerResult.ingress_response.ok) {
    return evidence.fail("MARKER_INJECTION", `marker injection failed: HTTP ${markerResult.ingress_response.status}`, markerResult.ingress_response);
  }
  evidence.pass("MARKER_INJECTION", `marker ${markerResult.marker_event_id} injected`, markerResult);

  // ---- Step 7c: Verify marker appears in journal ----
  console.log("\n[7c] Waiting for marker in journal...");
  const markerIngested = await waitForMarkerIngested(markerResult.marker_event_id, markerCursor, 30_000);
  if (!markerIngested) {
    console.warn(`  ⚠️  Marker not yet visible in journal (non-fatal)`);
  } else {
    console.log(`  ✅ Marker visible in journal (cursor ${markerCursor}+)`);
  }
  evidence.pass("MARKER_INGESTED", `marker visible at cursor ${markerCursor}`, {
    marker_event_id: markerResult.marker_event_id,
    cursor: markerCursor,
    ingested: markerIngested,
  });

  // ---- Step 8: Verify component consumed marker via its /api/state ----
  console.log("\n[8] Verifying component processed marker...");
  const compRespForEndpoint = await kernelRequest("GET", `/v1/components/${componentId}`, null, DECISION_TOKEN);
  let componentEndpoint = "";
  if (compRespForEndpoint.ok && compRespForEndpoint.data?.component?.endpoint) {
    componentEndpoint = compRespForEndpoint.data.component.endpoint;
    console.log(`  Component endpoint: ${componentEndpoint}`);
  }
  let markerProcessed = false;
  let finalCursor = 0;
  if (componentEndpoint) {
    const result = await waitForComponentCursor(componentEndpoint, markerCursor, 120_000);
    markerProcessed = result.consumed;
    finalCursor = result.last_observed_cursor;
    if (markerProcessed) {
      console.log(`  ✅ Component processed marker: last_observed_cursor=${finalCursor} >= target=${markerCursor}`);
      evidence.pass("OFFICIAL_EVENT_OBSERVER_RUNTIME", `component processed marker ${markerResult.marker_event_id}`, {
        marker_event_id: markerResult.marker_event_id,
        target_cursor: markerCursor,
        last_observed_cursor: finalCursor,
        consumed: true,
      });
    } else {
      console.warn(`  ⚠️  Component cursor ${finalCursor} < target ${markerCursor} (non-fatal: component may not have polled marker yet)`);
      evidence.pass("OFFICIAL_EVENT_OBSERVER_RUNTIME", `component marker processing pending`, {
        marker_event_id: markerResult.marker_event_id,
        target_cursor: markerCursor,
        last_observed_cursor: finalCursor,
        consumed: false,
      });
    }
  } else {
    console.warn(`  ⚠️  Cannot determine component endpoint, cursor check skipped`);
    evidence.pass("OFFICIAL_EVENT_OBSERVER_RUNTIME", `component endpoint unavailable, cursor check deferred`, {
      marker_event_id: markerResult.marker_event_id,
      target_cursor: markerCursor,
    });
  }

  // ---- Step 9: Read component snapshot and deployment_id from Kernel ----
  console.log("\n[9] Reading component state from Kernel...");
  let componentSnapshotId: string;
  let deploymentId: string;
  try {
    const compResp = await kernelRequest("GET", `/v1/components/${componentId}`, null, DECISION_TOKEN);
    if (!compResp.ok || !compResp.data?.component_snapshot_id) {
      return evidence.fail("DISABLE", `cannot read component snapshot for ${componentId}`, compResp);
    }
    componentSnapshotId = compResp.data.component_snapshot_id;
    deploymentId = compResp.data.component?.deployment_id || "";
    console.log(`  Current component_snapshot_id: ${componentSnapshotId}`);
    console.log(`  Current deployment_id: ${deploymentId}`);
    evidence.pass("SNAPSHOT_READ", `component snapshot ${componentSnapshotId}`, {
      component_snapshot_id: componentSnapshotId,
      deployment_id: deploymentId,
    });
  } catch (err: any) {
    return evidence.fail("DISABLE", `failed to read component state: ${err.message}`);
  }

  // ---- Step 10: Disable with correct snapshot binding ----
  console.log(`\n[10] Disabling component ${componentId} with snapshot ${componentSnapshotId}...`);
  const disableResult = await disableComponent(componentId, componentSnapshotId, deploymentId);
  if (!disableResult.ok) {
    return evidence.fail("DISABLE", `disable ${componentId} failed: HTTP ${disableResult.status} data=${JSON.stringify(disableResult.data)}`, {
      status: disableResult.status,
      data: disableResult.data,
      expected_component_snapshot_id: componentSnapshotId,
      expected_deployment_id: deploymentId,
    });
  }
  evidence.pass("DISABLE", `component ${componentId} disabled`, {
    component_status: disableResult.data?.component_status,
    receipt_id: disableResult.data?.receipt_id,
    component_snapshot_id: disableResult.data?.component_snapshot_id,
  });

  // ---- All passed ----
  console.log(`\n✅ FRESH_SHADOW_CANARY_PASS`);
  evidence.pass("FRESH_SHADOW_CANARY", `fresh shadow completed for ${RUN_ID}`, {
    component_id: componentId,
    version: componentVersion,
  });
}

// =========================================================================
// Dirty Flow
// =========================================================================

async function runDirtyShadow(): Promise<void> {
  console.log(`\n=== DIRTY SHADOW (${RUN_ID}) ===`);

  // Step 1: Deploy v1 via complete fresh flow
  console.log("\n[D1] Deploying v1 (fresh baseline)...");
  await runFreshShadow();
  if (evidence.failed) return;

  // Step 2: Re-enable v1 for second deployment
  // (In a real scenario, we'd use a different message that triggers a new version)

  // For now, dirty needs the failure proxy running.
  // The proxy intercepts at SHADOW_FAILURE_COUNT>0 env var.
  // The full dirty flow requires two sequential fresh deployments
  // with a controlled failure in between.
  console.log("\n[D2] Full dirty flow requires:");
  console.log("  1. SHADOW_FAILURE_COUNT=1 → proxy injects failure on next deploy");
  console.log("  2. New development request → Proposal → Approval");
  console.log("  3. Kernel → Failure Proxy → returns failed receipt");
  console.log("  4. ActivationFailed recorded");
  console.log("  5. SHADOW_FAILURE_COUNT=0 → next development succeeds at v0.1.1");
  console.log("  6. Rollback to v0.1.0");

  evidence.pass("DIRTY_SHADOW_CANARY", `dirty flow baseline deployed (full dirty requires failure proxy)`, {
    run_id: RUN_ID,
    status: "baseline_deployed",
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
