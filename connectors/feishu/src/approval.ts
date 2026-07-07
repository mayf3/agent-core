/**
 * Feishu Text Approval Adapter v0
 *
 * Intercepts approval/rejection commands from Feishu private chat,
 * fetches the proposal details (including digests) from the Kernel's
 * read-only GET endpoint, then calls the Kernel Decision API with
 * the real artifact_digest and manifest_digest.
 *
 * The decision token is only accessible in this module and is never
 * exposed to the LLM context, Coding Harness, or Capability Host.
 */

import { renderDecisionApproved, renderDecisionRejected, renderError } from "./renderer.js";

export interface ApprovalConfig {
  /** Kernel base URL, e.g. http://127.0.0.1:4130 */
  kernelBaseUrl: string;
  /** Bearer token for the Kernel Decision API.  NEVER expose to LLM/harness. */
  decisionToken: string | undefined;
  /** The Feishu open_id of the authorised owner. */
  ownerOpenId: string | undefined;
}

export interface ApprovalCommand {
  kind: "approve" | "reject";
  proposalId: string;
  reason: string;
}

export interface ProposalInfo {
  proposal_id: string;
  status: string;
  operation_name: string;
  manifest_id: string;
  artifact_digest: string;
  manifest_digest: string;
  endpoint?: string;
  risk?: string;
}

export interface ApprovalResult {
  ok: boolean;
  replyText: string;
  proposalInfo?: ProposalInfo;
}

// ── Command parsing ────────────────────────────────────────────────

const APPROVE_RE = /^批准\s+([a-zA-Z0-9_-]+)$/;
const REJECT_RE = /^拒绝\s+([a-zA-Z0-9_-]+)\s+(.+)/;

export function parseApprovalCommand(text: string): ApprovalCommand | null {
  const trimmed = text.trim();
  let m = trimmed.match(APPROVE_RE);
  if (m) {
    return { kind: "approve", proposalId: m[1], reason: "" };
  }
  m = trimmed.match(REJECT_RE);
  if (m) {
    return { kind: "reject", proposalId: m[1], reason: m[2].trim() };
  }
  return null;
}

// ── Authorisation ──────────────────────────────────────────────────

export function isApprovalAuthorised(
  config: ApprovalConfig,
  chatType: string,
  senderOpenId: string,
): string | null {
  if (!config.decisionToken) {
    return "审批功能未配置 (decision token 缺失)";
  }
  if (!config.ownerOpenId) {
    return "审批功能未配置 (owner 未设置)";
  }
  if (chatType !== "p2p") {
    return "审批仅支持私聊";
  }
  if (senderOpenId !== config.ownerOpenId) {
    return "您不是审批人";
  }
  return null;
}

// ── Kernel API helpers ─────────────────────────────────────────────

async function kernelFetch(
  config: ApprovalConfig,
  path: string,
  method: string,
  body?: unknown,
): Promise<{ ok: boolean; status: number; data: any }> {
  const url = `${config.kernelBaseUrl}${path}`;
  const headers: Record<string, string> = {
    Authorization: `Bearer ${config.decisionToken!}`,
  };
  if (body) {
    headers["Content-Type"] = "application/json";
  }
  const response = await fetch(url, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await response.json().catch(() => ({ error: "invalid_json_response" }));
  return { ok: response.ok, status: response.status, data };
}

/**
 * Fetch proposal details from the Kernel's read-only GET endpoint.
 */
export async function fetchProposal(
  config: ApprovalConfig,
  proposalId: string,
): Promise<ProposalInfo> {
  const { ok, status, data } = await kernelFetch(
    config,
    `/v1/capability-change-proposals/${encodeURIComponent(proposalId)}`,
    "GET",
  );
  if (!ok) {
    const errMsg = String(data.error || "unknown_error");
    if (status === 404 || errMsg.includes("not_found")) {
      throw new ApprovalError(`Proposal ${proposalId} 不存在`, "proposal_not_found");
    }
    if (status === 401 || errMsg.includes("unauthorized")) {
      throw new ApprovalError("审批授权失败", "unauthorized");
    }
    throw new ApprovalError(`获取 proposal 失败: ${renderError(errMsg)}`, "api_error");
  }
  if (!data.proposal_id) {
    throw new ApprovalError("获取 proposal 返回格式异常", "invalid_response");
  }
  return data as ProposalInfo;
}

// ── Domain errors ──────────────────────────────────────────────────

export class ApprovalError extends Error {
  constructor(
    message: string,
    public readonly code: string,
  ) {
    super(message);
    this.name = "ApprovalError";
  }
}

// ── Decision execution ─────────────────────────────────────────────

/**
 * Execute an approval command against the Kernel Decision API.
 * Fetches proposal details first to obtain real digests.
 */
export async function executeApprovalCommand(
  config: ApprovalConfig,
  command: ApprovalCommand,
): Promise<ApprovalResult> {
  // 1. Fetch proposal details to get real digests and verify status.
  let proposal: ProposalInfo;
  try {
    proposal = await fetchProposal(config, command.proposalId);
  } catch (err) {
    const msg = err instanceof ApprovalError ? err.message : String(err);
    return { ok: false, replyText: msg };
  }

  // 2. Verify proposal is still PendingApproval.
  if (proposal.status !== "PendingApproval") {
    return {
      ok: false,
      replyText: `Proposal ${command.proposalId} 状态为 ${proposal.status}，无法审批`,
      proposalInfo: proposal,
    };
  }

  // 3. Build decision payload with real digests.
  const decision = command.kind === "approve" ? "approved" : "rejected";
  const { ok, data } = await kernelFetch(
    config,
    `/v1/capability-change-proposals/${encodeURIComponent(command.proposalId)}/decision`,
    "POST",
    {
      decision,
      artifact_digest: proposal.artifact_digest,
      manifest_digest: proposal.manifest_digest,
    },
  );

  if (ok) {
    if (command.kind === "approve") {
      return {
        ok: true,
        replyText: renderDecisionApproved({
          proposal_id: command.proposalId,
          activated_snapshot_id: data.activated_snapshot_id,
          manifest_id: proposal.manifest_id,
        }),
        proposalInfo: proposal,
      };
    }
    return {
      ok: true,
      replyText: renderDecisionRejected({
        proposal_id: command.proposalId,
        reason: command.reason || undefined,
      }),
      proposalInfo: proposal,
    };
  }

  // 4. Handle API errors.
  const errorMsg = String(data.error || "unknown_error");
  return {
    ok: false,
    replyText: `${command.kind === "approve" ? "批准" : "拒绝"}失败: ${renderError(errorMsg)}`,
    proposalInfo: proposal,
  };
}
