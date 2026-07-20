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

import {
  renderDecisionApproved,
  renderDecisionCard,
  renderDecisionRejected,
  renderError,
  renderProposalPendingCard,
} from "./renderer.js";
import type { FeishuTransport } from "./transport.js";

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
  approval: ProposalApproval;
}

export interface ProposalApproval {
  approval_id: string;
  principal_id: string;
  expected_source_snapshot_id: string;
  candidate_digest: string;
  artifact_digest: string;
  manifest_digest: string;
  decision_nonce: string;
  expires_at: string;
  status: string;
  origin_channel: string;
  origin_conversation_kind: string;
}

export interface ApprovalResult {
  ok: boolean;
  replyText: string;
  proposalInfo?: ProposalInfo;
  decisionId?: string;
  activatedSnapshotId?: string;
  hostDeploymentId?: string;
  componentUrl?: string;
}

export interface ProposalDecision {
  kind: "approve" | "reject";
  proposalId: string;
  actorPrincipalId: string;
  reason?: string;
  expectedApprovalId?: string;
  expectedDecisionNonce?: string;
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
  if (!data.proposal_id || data.proposal_id !== proposalId) {
    throw new ApprovalError("获取 proposal 返回格式异常", "invalid_response");
  }
  assertApprovalBinding(data.approval);
  if (
    data.approval.artifact_digest !== data.artifact_digest ||
    data.approval.manifest_digest !== data.manifest_digest ||
    data.approval.principal_id !== principalForOpenId(config.ownerOpenId || "")
  ) {
    throw new ApprovalError("Proposal 审批绑定不一致", "invalid_approval_binding");
  }
  return data as ProposalInfo;
}

function assertApprovalBinding(value: any): asserts value is ProposalApproval {
  const fields = [
    "approval_id",
    "principal_id",
    "expected_source_snapshot_id",
    "candidate_digest",
    "artifact_digest",
    "manifest_digest",
    "decision_nonce",
    "expires_at",
    "status",
    "origin_channel",
    "origin_conversation_kind",
  ];
  if (!value || fields.some((field) => typeof value[field] !== "string" || !value[field])) {
    throw new ApprovalError("Proposal 缺少完整审批绑定", "invalid_approval_binding");
  }
  if (value.origin_channel !== "Feishu" || value.origin_conversation_kind !== "p2p") {
    throw new ApprovalError("审批仅支持私聊来源的 Proposal", "invalid_approval_origin");
  }
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
  actorPrincipalId: string,
): Promise<ApprovalResult> {
  return executeProposalDecision(config, {
    kind: command.kind,
    proposalId: command.proposalId,
    actorPrincipalId,
    reason: command.reason,
  });
}

export function principalForOpenId(openId: string): string {
  return openId ? `feishu:open_id:${openId}` : "";
}

function approvalOperatorError(config: ApprovalConfig, operatorOpenId: string): string | null {
  if (!config.decisionToken) return "审批功能未配置 (decision token 缺失)";
  if (!config.ownerOpenId) return "审批功能未配置 (owner 未设置)";
  if (operatorOpenId !== config.ownerOpenId) return "您不是审批人";
  return null;
}

/** Execute one fully identity-bound decision. Kernel remains authoritative. */
export async function executeProposalDecision(
  config: ApprovalConfig,
  decisionInput: ProposalDecision,
): Promise<ApprovalResult> {
  // 1. Fetch the authoritative binding on every click/text command.
  let proposal: ProposalInfo;
  try {
    proposal = await fetchProposal(config, decisionInput.proposalId);
  } catch (err) {
    const msg = err instanceof ApprovalError ? err.message : String(err);
    return { ok: false, replyText: msg };
  }

  // 2. Terminal states must still reach Kernel so an identical callback can
  // replay its durable result and a conflicting callback can be rejected.
	  const replayableProposalStatuses = new Set([
	    "PendingApproval", "Approved", "Rejected", "Activated", "ActivationFailed",
	    "deployment_pending",
	  ]);
  if (!replayableProposalStatuses.has(proposal.status)) {
    return {
      ok: false,
      replyText: `Proposal ${decisionInput.proposalId} 状态为 ${proposal.status}，无法审批`,
      proposalInfo: proposal,
    };
  }
  const replayableApprovalStatuses = new Set([
    "Pending", "Approved", "Rejected", "ActivationFailed",
  ]);
  if (!replayableApprovalStatuses.has(proposal.approval.status)) {
    return {
      ok: false,
      replyText: `Approval ${proposal.approval.approval_id} 状态为 ${proposal.approval.status}，无法审批`,
      proposalInfo: proposal,
    };
  }

  if (decisionInput.expectedApprovalId && decisionInput.expectedApprovalId !== proposal.approval.approval_id) {
    return { ok: false, replyText: "审批卡片已失效，请使用最新卡片", proposalInfo: proposal };
  }
  if (decisionInput.expectedDecisionNonce && decisionInput.expectedDecisionNonce !== proposal.approval.decision_nonce) {
    return { ok: false, replyText: "审批卡片 nonce 已失效，请使用最新卡片", proposalInfo: proposal };
  }
  if (!decisionInput.actorPrincipalId) {
    return { ok: false, replyText: "审批人身份缺失", proposalInfo: proposal };
  }

  // 3. Forward every authoritative binding field. The edge never decides.
  const decision = decisionInput.kind === "approve" ? "approved" : "rejected";
  let response: { ok: boolean; data: any };
  try {
    response = await kernelFetch(
      config,
      `/v1/capability-change-proposals/${encodeURIComponent(decisionInput.proposalId)}/decision`,
      "POST",
      {
        decision,
        approval_id: proposal.approval.approval_id,
        decision_nonce: proposal.approval.decision_nonce,
        principal_id: decisionInput.actorPrincipalId,
        expected_source_snapshot_id: proposal.approval.expected_source_snapshot_id,
        candidate_digest: proposal.approval.candidate_digest,
        artifact_digest: proposal.approval.artifact_digest,
        manifest_digest: proposal.approval.manifest_digest,
      },
    );
  } catch {
    return { ok: false, replyText: "审批服务暂时不可用", proposalInfo: proposal };
  }
  const { ok, data } = response;

	  if (ok) {
	    if (
	      decisionInput.kind === "approve" &&
	      data.approval_id === proposal.approval.approval_id &&
	      typeof data.decision_id === "string" &&
	      data.decision_id &&
	      data.status === "ActivationFailed"
	    ) {
	      return {
	        ok: false,
	        replyText: `批准已记录，但能力激活失败: ${renderError(String(data.activation_error || "activation_failed"))}`,
	        proposalInfo: proposal,
	        decisionId: data.decision_id,
	      };
	    }
	    // Async approval: the kernel accepted the approval and started
	    // deployment in the background. Return an immediate ACK; the
	    // final result arrives via the existing notification mechanism.
	    if (
	      decisionInput.kind === "approve" &&
	      data.status === "deployment_pending"
	    ) {
	      return {
	        ok: true,
	        replyText: "批准已受理，能力正在激活",
	        proposalInfo: proposal,
	        decisionId: data.decision_id,
	      };
	    }
	    const expectedStatus = decisionInput.kind === "approve" ? "Activated" : "Rejected";
	    const invalidIdentity =
	      data.approval_id !== proposal.approval.approval_id ||
	      typeof data.decision_id !== "string" ||
	      !data.decision_id ||
	      data.status !== expectedStatus;
	    const invalidActivation = decisionInput.kind === "approve" && (
	      typeof data.activated_snapshot_id !== "string" || !data.activated_snapshot_id ||
	      typeof data.host_deployment_id !== "string" || !data.host_deployment_id
	    );
    if (invalidIdentity || invalidActivation) {
      return { ok: false, replyText: "审批服务返回格式异常", proposalInfo: proposal };
    }
    if (decisionInput.kind === "approve") {
      const componentUrl = typeof data.component_url === "string" &&
        /^http:\/\/(127\.0\.0\.1|\[::1\]):\d+$/.test(data.component_url)
        ? data.component_url
        : undefined;
      return {
        ok: true,
        replyText: renderDecisionApproved({
          proposal_id: decisionInput.proposalId,
          decision_id: data.decision_id,
          activated_snapshot_id: data.activated_snapshot_id,
          manifest_id: proposal.manifest_id,
          component_url: componentUrl,
        }),
        proposalInfo: proposal,
        decisionId: data.decision_id,
        activatedSnapshotId: data.activated_snapshot_id,
        hostDeploymentId: data.host_deployment_id,
        componentUrl,
      };
    }
    return {
      ok: true,
      replyText: renderDecisionRejected({
        proposal_id: decisionInput.proposalId,
        reason: decisionInput.reason || undefined,
      }),
      proposalInfo: proposal,
      decisionId: data.decision_id,
    };
  }

  // 4. Handle API errors.
  const errorMsg = String(data.error || "unknown_error");
  return {
    ok: false,
    replyText: `${decisionInput.kind === "approve" ? "批准" : "拒绝"}失败: ${renderError(errorMsg)}`,
    proposalInfo: proposal,
  };
}

export interface PendingProposalPresentation {
  kind: "capability_proposal_pending_v1";
  proposal_id: string;
}

export function parsePendingProposalPresentation(value: unknown): PendingProposalPresentation | null {
  const input = value as Record<string, unknown> | null;
  if (
    !input ||
    Object.keys(input).length !== 2 ||
    input.kind !== "capability_proposal_pending_v1" ||
    typeof input.proposal_id !== "string" ||
    !input.proposal_id
  ) return null;
  return { kind: input.kind, proposal_id: input.proposal_id };
}

export async function sendPendingProposalCardReply(
  transport: FeishuTransport,
  messageId: string,
  presentation: PendingProposalPresentation,
  config: ApprovalConfig,
) {
  const proposal = await fetchProposal(config, presentation.proposal_id);
  if (proposal.status !== "PendingApproval" || proposal.approval.status !== "Pending") {
    throw new Error("proposal_not_pending");
  }
  const card = renderProposalPendingCard({
    proposal_id: proposal.proposal_id,
    operation_name: proposal.operation_name,
    artifact_digest: proposal.approval.artifact_digest,
    approval_id: proposal.approval.approval_id,
    decision_nonce: proposal.approval.decision_nonce,
  });
  return transport.replyToMessage(messageId, "interactive", card);
}

export function parseProposalCardAction(raw: any) {
  const event = raw?.event || raw;
  const operatorOpenId = String(event?.operator?.open_id || "");
  let value = event?.action?.value;
  if (typeof value === "string") {
    try { value = JSON.parse(value); } catch { return null; }
  }
  if (!value || typeof value !== "object") return null;
  const input = value as Record<string, unknown>;
  if (
    typeof input.proposal_id !== "string" || !input.proposal_id ||
    typeof input.approval_id !== "string" || !input.approval_id ||
    typeof input.decision_nonce !== "string" || !input.decision_nonce ||
    (input.decision !== "approved" && input.decision !== "rejected") || !operatorOpenId
  ) return null;
  return {
    proposalId: input.proposal_id,
    approvalId: input.approval_id,
    decisionNonce: input.decision_nonce,
    decision: input.decision,
    operatorOpenId,
  };
}

export async function handleProposalCardAction(config: ApprovalConfig, raw: unknown) {
  const action = parseProposalCardAction(raw);
  if (!action) return cardCallbackError("invalid_card_action");
  // Card callbacks do not expose a trustworthy chat_type. Never synthesize
  // one: authenticate the operator here; fetchProposal independently requires
  // the Kernel-authoritative Proposal origin to be Feishu/p2p.
  const authError = approvalOperatorError(config, action.operatorOpenId);
  if (authError) return cardCallbackError(authError, action.proposalId);
  const result = await executeProposalDecision(config, {
    kind: action.decision === "approved" ? "approve" : "reject",
    proposalId: action.proposalId,
    actorPrincipalId: principalForOpenId(action.operatorOpenId),
    expectedApprovalId: action.approvalId,
    expectedDecisionNonce: action.decisionNonce,
  });
  if (!result.ok) return cardCallbackError(result.replyText, action.proposalId);
  const approved = action.decision === "approved";
  return {
    toast: { type: "success", content: approved ? "APPROVED" : "REJECTED" },
    card: {
      type: "raw",
      data: renderDecisionCard({
        approved,
        proposal_id: action.proposalId,
        decision_id: result.decisionId,
        activated_snapshot_id: result.activatedSnapshotId,
        component_url: result.componentUrl,
      }),
    },
  };
}

function cardCallbackError(error: string, proposalId = "unknown") {
  return {
    toast: { type: "error", content: "审批失败" },
    card: {
      type: "raw",
      data: renderDecisionCard({ approved: false, proposal_id: proposalId, error }),
    },
  };
}
