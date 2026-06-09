import {
  appendEvent,
  createApprovalRequest,
  createEvent,
  createRunRecord,
  decideApproval,
  errorEnvelope,
  getApproval,
  okEnvelope,
  recordApproval,
  recordRun,
  updateRunStatus,
} from "../../core/src/index.mjs";
import { evaluateToolPolicy } from "./policy.mjs";
import { createToolRegistry } from "./registry.mjs";
import { buildToolContext } from "./sandbox.mjs";

export async function runTool(input = {}) {
  const started = Date.now();
  const registry = input.registry || createToolRegistry();
  const tool = registry.get(input.toolName);
  if (!tool) {
    return errorEnvelope({ code: "tool_not_found", message: `Unknown tool: ${input.toolName}` });
  }

  const context = buildToolContext(input);
  const run = input.runId ? { runId: input.runId } : createRunRecord({
    prefix: "tool",
    source: input.source || "cli",
    inputSummary: `${tool.name}`,
    status: "running",
  });
  if (!input.runId) {
    await recordRun(context.stateDir, run);
    await appendEvent(context.stateDir, createEvent("run.started", { runId: run.runId, source: run.source }));
  }

  const policy = evaluateToolPolicy(tool, input.args, context, input.approval);
  if (policy.decision === "deny") {
    return failTool(context, run.runId, "tool_policy_denied", policy.reason, started);
  }
  if (policy.decision === "needs_approval") {
    return requestToolApproval(context, run.runId, tool, input.args, policy, started);
  }

  return executeAllowedTool(context, run.runId, tool, input.args, started, input.manageRunStatus !== false);
}

export async function resumeApproval(input = {}) {
  const approval = await getApproval(input.stateDir, input.approvalId);
  if (!approval) {
    return errorEnvelope({ code: "approval_not_found", message: `Unknown approval: ${input.approvalId}` });
  }
  if (approval.status !== "pending") {
    return errorEnvelope({
      runId: approval.runId,
      code: "approval_already_decided",
      message: `Approval is already ${approval.status}.`,
    });
  }
  const decision = input.decision === "approved" ? "approved" : "rejected";
  const decided = await decideApproval(input.stateDir, approval.approvalId, decision, { decidedBy: input.decidedBy });
  await appendEvent(input.stateDir, createEvent("approval.decided", {
    runId: approval.runId,
    approvalId: approval.approvalId,
    decision,
  }));
  if (decision !== "approved") {
    await updateRunStatus(input.stateDir, approval.runId, "cancelled", { resultSummary: "approval rejected" });
    return okEnvelope({ runId: approval.runId, status: "cancelled", result: { approval: decided } });
  }
  return runTool({ ...approval.resume, stateDir: input.stateDir, runId: approval.runId, approval: decided });
}

async function requestToolApproval(context, runId, tool, args, policy, started) {
  const approval = createApprovalRequest({
    runId,
    requestedAction: `${tool.name}`,
    riskLevel: policy.riskLevel,
    reason: policy.reason,
    policyVersion: policy.policyVersion,
    resume: { toolName: tool.name, args, workspace: context.workspace, cwd: context.cwd },
  });
  await recordApproval(context.stateDir, approval);
  const event = await appendEvent(context.stateDir, createEvent("approval.requested", {
    runId,
    approvalId: approval.approvalId,
    toolName: tool.name,
    riskLevel: approval.riskLevel,
  }));
  await updateRunStatus(context.stateDir, runId, "needs_approval", { resultSummary: approval.reason });
  return okEnvelope({
    runId,
    status: "needs_approval",
    result: { approval },
    events: [event],
    usage: { elapsedMs: Date.now() - started },
  });
}

async function executeAllowedTool(context, runId, tool, args, started, manageRunStatus) {
  await appendEvent(context.stateDir, createEvent("tool.called", { runId, toolName: tool.name }));
  try {
    const output = await tool.execute(args, context);
    const event = await appendEvent(context.stateDir, createEvent("tool.completed", { runId, toolName: tool.name }));
    if (manageRunStatus) {
      await updateRunStatus(context.stateDir, runId, "ok", { resultSummary: `${tool.name} completed` });
    }
    return okEnvelope({
      runId,
      result: { type: "tool-result", toolName: tool.name, output },
      events: [event],
      usage: { elapsedMs: Date.now() - started },
    });
  } catch (error) {
    return failTool(context, runId, "tool_failed", error instanceof Error ? error.message : String(error), started);
  }
}

async function failTool(context, runId, code, message, started) {
  const event = await appendEvent(context.stateDir, createEvent("tool.failed", { runId, code, message }));
  await updateRunStatus(context.stateDir, runId, "failed", { resultSummary: message });
  return errorEnvelope({ runId, code, message, events: [event], usage: { elapsedMs: Date.now() - started } });
}
