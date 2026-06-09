import { assertInsideWorkspace } from "./sandbox.mjs";

const policyVersion = "policy.v1";
const dangerousCommand = /(rm\s+-rf\s+\/|sudo\b|mkfs\b|dd\s+if=|chmod\s+-R\s+777|chown\s+-R)/i;

export function evaluateToolPolicy(tool, args = {}, context = {}, approval = null) {
  try {
    assertInsideWorkspace(context.workspace, context.cwd);
    if (args.path) {
      assertInsideWorkspace(context.workspace, args.path);
    }
  } catch (error) {
    return deny(error.message);
  }

  if (tool.name === "shell.exec" && dangerousCommand.test(String(args.cmd || ""))) {
    return deny("Command matches dangerous command denylist.");
  }

  if (tool.name === "http.fetch" && context.network !== "allow" && !isApproved(approval)) {
    return requireApproval("medium", "Network access requires approval.");
  }

  if ((tool.permission === "write" || tool.permission === "execute" || tool.permission === "dangerous") && !isApproved(approval)) {
    return requireApproval(tool.permission === "dangerous" ? "high" : "medium", `${tool.name} requires approval.`);
  }

  return { ok: true, decision: "allow", policyVersion };
}

function isApproved(approval) {
  return approval?.status === "approved" || approval?.decision === "approved";
}

function deny(reason) {
  return { ok: false, decision: "deny", reason, policyVersion };
}

function requireApproval(riskLevel, reason) {
  return { ok: true, decision: "needs_approval", riskLevel, reason, policyVersion };
}
