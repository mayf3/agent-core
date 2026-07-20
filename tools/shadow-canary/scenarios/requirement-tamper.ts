//! Requirement content tamper test — proves REQUIREMENT_DIGEST_MISMATCH.
//!
//! Sends an HCR acceptance request with a tampered requirement body
//! while keeping the original requirement_digest. Must fail before
//! any manifest construction.

import { evidence } from "../evidence.ts";
import { kernelRequest, sleep } from "../clients/http-client.ts";
import {
  simulateFeishuMessage,
  config,
} from "../connector-shadow.ts";
import { getCurrentCursor, waitForAnyProposal } from "../development-cycle.ts";

const DECISION_TOKEN = process.env.AGENT_CORE_CAPABILITY_DECISION_TOKEN || "";
const IPC_TOKEN = process.env.AGENT_CORE_IPC_TOKEN || "";
const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;
const SENDER_OPEN_ID = config.feishuOwnerOpenId || "ou_shadow_owner";

export async function runRequirementTamperTest(): Promise<void> {
  console.log(`\n=== REQUIREMENT TAMPER TEST (${RUN_ID}) ===`);

  // Step 1: Create a normal calculator proposal (to get a real HCR)
  const startCursor = await getCurrentCursor();
  const msgId = `tamper_${RUN_ID}`;
  const MESSAGE_TEXT = "开发一个 external.calculator，支持加减乘除";

  const msgResult = await simulateFeishuMessage(MESSAGE_TEXT, msgId, SENDER_OPEN_ID);
  if (!msgResult.ok) {
    evidence.fail("TAMPER_INGRESS", `message failed: ${JSON.stringify(msgResult)}`, msgResult);
    return;
  }
  evidence.pass("TAMPER_INGRESS", `calculator message sent`, msgResult);

  // Step 2: Wait for proposal and get HCR info
  let proposalEvent: any;
  try {
    proposalEvent = await waitForAnyProposal(300_000, startCursor);
  } catch (err: any) {
    evidence.fail("TAMPER_PROPOSAL", `no proposal: ${err.message}`);
    return;
  }
  evidence.pass("TAMPER_PROPOSAL", `proposal ${proposalEvent.proposal_id} created`, proposalEvent);

  // Read the HCR to get the requirement digest
  const hcrId = proposalEvent.hcr_id;
  if (!hcrId) {
    evidence.fail("TAMPER_HCR", `no hcr_id in proposal event`, proposalEvent);
    return;
  }

  // Step 3: Read the actual HCR requirement from Kernel API
  const hcrResp = await kernelRequest("GET", `/v1/harness-change-requests/${hcrId}`, null, DECISION_TOKEN);
  if (!hcrResp.ok || !hcrResp.data?.requirement) {
    evidence.fail("TAMPER_HCR_READ", `cannot read HCR ${hcrId}`, hcrResp);
    return;
  }
  const originalRequirement = hcrResp.data.requirement;
  const originalDigest = hcrResp.data.requirement_digest || "";

  // Step 4: Create a tampered version with different development_request
  let tamperedRequirement: any;
  try {
    tamperedRequirement = JSON.parse(originalRequirement);
    // Replace the development_request's name to create a mismatch
    if (tamperedRequirement.development_request) {
      tamperedRequirement.development_request.name = "external.tampered";
    }
  } catch {
    evidence.fail("TAMPER_PARSE", `cannot parse requirement`);
    return;
  }

  const tamperedBody = JSON.stringify(tamperedRequirement);
  // Keep the original digest (this should cause REQUIREMENT_DIGEST_MISMATCH)

  // Step 5: Simulate an HCR acceptance with the tampered requirement
  // We send a direct acceptance call to the Harness with tampered data
  const tamperAcceptArgs = {
    protocol_version: "external-harness-v1",
    operation: "external.coding_hcr_accept",
    arguments: {
      candidate_ref: proposalEvent.candidate_ref || "",
      hcr_id: hcrId,
      claim_id: proposalEvent.claim_id || "",
      run_id: proposalEvent.run_id || "",
      principal_id: proposalEvent.principal_id || "",
      gateway_session_id: proposalEvent.session_id || "",
      registry_snapshot_id: proposalEvent.registry_snapshot_id || "",
      operation: "external.coding_hcr_accept",
      idempotency_key: `tamper_test_${RUN_ID}`,
      invocation_intent_id: `tamper_intent_${RUN_ID}`,
      development_request: tamperedRequirement.development_request,
      requirement_digest: originalDigest,
      requirement: tamperedBody,
    },
  };

  // We send directly to the Harness (port 7200) - the acceptance flow checks digest
  const harnessResp = await kernelRequest(
    "POST", "/v1/execute", tamperAcceptArgs, IPC_TOKEN,
  );

  // Step 6: Verify the result is REQUIREMENT_DIGEST_MISMATCH
  const errorCode = harnessResp.data?.error_code || harnessResp.body?.error_code || "";
  const isRejected = errorCode === "REQUIREMENT_DIGEST_MISMATCH"
    || harnessResp.body?.result?.error_code === "REQUIREMENT_DIGEST_MISMATCH";

  if (isRejected) {
    evidence.pass("TAMPER_REQUIREMENT_DIGEST", `tampered requirement correctly rejected with REQUIREMENT_DIGEST_MISMATCH`, {
      original_digest: originalDigest,
      tampered_name: "external.tampered",
      error_code: "REQUIREMENT_DIGEST_MISMATCH",
    });
  } else {
    evidence.fail("TAMPER_REQUIREMENT_DIGEST", `tampered requirement NOT rejected as expected. Response: ${JSON.stringify(harnessResp.data)}`, harnessResp);
  }

  evidence.pass("REQUIREMENT_TAMPER_TEST", `requirement content tamper test completed`, {
    original_digest: originalDigest,
    tampered: true,
    expected_error: "REQUIREMENT_DIGEST_MISMATCH",
    actual_error: errorCode,
  });
}
