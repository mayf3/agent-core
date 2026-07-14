/**
 * Feishu Presentation Renderer v0
 *
 * Pure rendering functions that transform structured events into
 * Feishu-friendly text.  No side effects, no token access, no
 * Kernel mutation calls.
 */

function truncate(value: string, max: number): string {
  if (value.length <= max) return value;
  return value.slice(0, max) + "…";
}

export function renderJson(value: unknown, maxLen = 200): string {
  const raw = JSON.stringify(value, null, 0);
  if (raw.length <= maxLen) return "`" + raw + "`";
  return "`" + raw.slice(0, maxLen - 10) + "…`";
}

export function renderJsonBlock(value: unknown): string {
  const raw = JSON.stringify(value, null, 2);
  const lines = raw.split("\n");
  const shown = lines.length > 40 ? [...lines.slice(0, 37), "  …", ...lines.slice(-2)] : lines;
  return "```json\n" + shown.join("\n") + "\n```";
}

export function renderError(message: string): string {
  const safe = message
    .replace(/(token|secret|key|password|auth)[_:]\s*\S+/gi, "$1: [REDACTED]")
    .replace(/Bearer\s+\S+/gi, "Bearer [REDACTED]");
  return truncate(safe, 500);
}

export function renderProposalPending(data: {
  proposal_id: string;
  operation_name: string;
  manifest_id: string;
  artifact_digest?: string;
  endpoint?: string;
  risk?: string;
}): string {
  const lines: string[] = ["📋 能力申请待审批"];
  lines.push(`Proposal: ${data.proposal_id}`);
  lines.push(`操作: ${data.operation_name}`);
  lines.push(`Manifest: ${data.manifest_id}`);
  if (data.artifact_digest) lines.push(`摘要: ${truncate(data.artifact_digest, 20)}`);
  if (data.endpoint) lines.push(`端点: ${data.endpoint}`);
  if (data.risk) lines.push(`类型: ${data.risk}`);
  lines.push("");
  lines.push(`回复"批准 ${data.proposal_id}" 来批准`);
  lines.push(`回复"拒绝 ${data.proposal_id} 理由" 来拒绝`);
  return lines.join("\n");
}

export interface PendingProposalCardData {
  proposal_id: string;
  operation_name: string;
  artifact_digest: string;
  approval_id: string;
  decision_nonce: string;
}

/** Render the fixed v0 calculator approval card from Kernel-authoritative data. */
export function renderProposalPendingCard(data: PendingProposalCardData): Record<string, unknown> {
  const actionValue = {
    proposal_id: data.proposal_id,
    approval_id: data.approval_id,
    decision_nonce: data.decision_nonce,
  };
  return {
    config: { wide_screen_mode: true },
    header: {
      template: "orange",
      title: { tag: "plain_text", content: "能力申请待审批" },
    },
    elements: [
      {
        tag: "div",
        text: {
          tag: "lark_md",
          content: [
            `**能力**：${truncate(data.operation_name, 80)}`,
            "**运算**：加 / 减 / 乘 / 除",
            `**Artifact**：${truncate(data.artifact_digest, 20)}`,
            `**Proposal**：${truncate(data.proposal_id, 80)}`,
          ].join("\n"),
        },
      },
      {
        tag: "action",
        actions: [
          {
            tag: "button",
            type: "primary",
            text: { tag: "plain_text", content: "批准" },
            value: { ...actionValue, decision: "approved" },
          },
          {
            tag: "button",
            type: "danger",
            text: { tag: "plain_text", content: "拒绝" },
            value: { ...actionValue, decision: "rejected" },
          },
        ],
      },
      {
        tag: "note",
        elements: [
          { tag: "plain_text", content: `文本降级：批准 ${truncate(data.proposal_id, 80)}` },
        ],
      },
    ],
  };
}

export function renderDecisionCard(data: {
  approved: boolean;
  proposal_id: string;
  decision_id?: string;
  activated_snapshot_id?: string;
  error?: string;
}): Record<string, unknown> {
  const outcome = data.error ? "ERROR" : data.approved ? "APPROVED" : "REJECTED";
  const lines = [
    `**${outcome}**`,
    `**Proposal**：${truncate(data.proposal_id, 80)}`,
  ];
  if (data.decision_id) lines.push(`**Decision ID**：${truncate(data.decision_id, 80)}`);
  if (data.activated_snapshot_id) {
    lines.push(`**新 Snapshot**：${truncate(data.activated_snapshot_id, 80)}`);
  }
  if (data.error) lines.push(`**结果**：${renderError(data.error)}`);
  return {
    config: { wide_screen_mode: true },
    header: {
      template: data.error ? "red" : data.approved ? "green" : "grey",
      title: {
        tag: "plain_text",
        content: data.error ? "审批失败" : data.approved ? "审批完成" : "审批未通过",
      },
    },
    elements: [{ tag: "div", text: { tag: "lark_md", content: lines.join("\n") } }],
  };
}

export function renderDecisionApproved(data: {
  proposal_id: string;
  decision_id?: string;
  activated_snapshot_id?: string;
  manifest_id?: string;
}): string {
  const lines: string[] = ["✅ APPROVED（已批准）"];
  lines.push(`Proposal: ${data.proposal_id}`);
  if (data.decision_id) lines.push(`Decision ID: ${truncate(data.decision_id, 80)}`);
  if (data.activated_snapshot_id) lines.push(`新 Snapshot: ${truncate(data.activated_snapshot_id, 20)}`);
  if (data.manifest_id) lines.push(`Manifest: ${truncate(data.manifest_id, 20)}`);
  return lines.join("\n");
}

export function renderDecisionRejected(data: { proposal_id: string; reason?: string }): string {
  const lines: string[] = ["❌ 已拒绝"];
  lines.push(`Proposal: ${data.proposal_id}`);
  if (data.reason) lines.push(`理由: ${data.reason}`);
  return lines.join("\n");
}

export function renderToolCall(operation: string, args: unknown): string {
  const lines: string[] = ["🛠 调用工具"];
  lines.push(operation);
  const argStr = renderJson(args, 150);
  if (argStr) lines.push(`参数: ${argStr}`);
  return lines.join("\n");
}

export function renderReceiptSucceeded(output: unknown): string {
  const lines: string[] = ["✅ 执行成功"];
  const rendered = renderJson(output, 200);
  if (rendered) lines.push(`结果: ${rendered}`);
  return lines.join("\n");
}

export function renderReceiptFailed(data: {
  error_category?: string;
  harness_error_code?: string;
  error_code?: string;
  http_code?: number;
  output?: unknown;
}): string {
  const lines: string[] = ["❌ 执行失败"];
  const cat = data.error_category || data.error_code || "";
  if (cat) lines.push(`错误类别: ${cat}`);
  const code = data.harness_error_code || "";
  if (code) lines.push(`错误码: ${code}`);
  if (data.http_code) lines.push(`HTTP 状态: ${data.http_code}`);
  if (data.output && typeof data.output === "object") {
    const extra = JSON.stringify(data.output).slice(0, 150);
    if (extra.length > 2) lines.push(`详情: ${extra}`);
  }
  return lines.join("\n");
}

export function splitLongText(text: string, maxLength = 1500): string[] {
  if (text.length <= maxLength) return [text];
  const parts: string[] = [];
  let remaining = text;
  while (remaining.length > 0) {
    let splitAt = maxLength;
    const newlineIdx = remaining.lastIndexOf("\n", maxLength);
    if (newlineIdx > 0) splitAt = newlineIdx;
    else {
      const spaceIdx = remaining.lastIndexOf(" ", maxLength);
      if (spaceIdx > 0) splitAt = spaceIdx;
    }
    parts.push(remaining.slice(0, splitAt));
    remaining = remaining.slice(splitAt).trim();
  }
  return parts;
}
