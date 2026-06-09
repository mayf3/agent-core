import { randomUUID } from "node:crypto";
import { appendStateRecord, readStateRecords, writeStateRecords } from "./store.mjs";

const approvalsFile = "approvals.jsonl";

export function createApprovalRequest(input = {}, now = new Date()) {
  return {
    approvalId: input.approvalId || `appr_${randomUUID()}`,
    runId: input.runId,
    status: "pending",
    requestedAction: input.requestedAction || "",
    riskLevel: input.riskLevel || "medium",
    reason: input.reason || "",
    policyVersion: input.policyVersion || "policy.v1",
    decision: null,
    decidedBy: null,
    requestedAt: now.toISOString(),
    decidedAt: null,
    resume: input.resume || null,
  };
}

export async function recordApproval(stateDir, approval) {
  await appendStateRecord(stateDir, approvalsFile, approval);
  return approval;
}

export async function readApprovals(stateDir, filter = {}) {
  const approvals = await readStateRecords(stateDir, approvalsFile);
  if (filter.status) {
    return approvals.filter((approval) => approval.status === filter.status);
  }
  return approvals;
}

export async function getApproval(stateDir, approvalId) {
  const approvals = await readApprovals(stateDir);
  return approvals.findLast((approval) => approval.approvalId === approvalId) || null;
}

export async function decideApproval(stateDir, approvalId, decision, options = {}) {
  const approvals = await readApprovals(stateDir);
  const index = approvals.findLastIndex((approval) => approval.approvalId === approvalId);
  if (index < 0) {
    return null;
  }
  const decided = {
    ...approvals[index],
    status: decision === "approved" ? "approved" : "rejected",
    decision,
    decidedBy: options.decidedBy || "local-user",
    decidedAt: new Date().toISOString(),
  };
  approvals[index] = decided;
  await writeStateRecords(stateDir, approvalsFile, approvals);
  return decided;
}
