/**
 * HarnessChangeRequest v0 — Feishu command parser and Kernel API caller.
 *
 * Intercepts "创建 Harness <id>：<requirement>" commands from Feishu private
 * chat, pre-filters owner/p2p, then calls the Kernel HCR endpoint.
 *
 * The Kernel independently re-validates auth — the Connector pre-filter is
 * a convenience, NOT a security boundary.
 */

import type { ConnectorConfig } from "./config.js";

export interface HCRCommand {
  harnessId: string;
  requirement: string;
}

export interface HCRResult {
  ok: boolean;
  replyText: string;
  requestId?: string;
  deduplicated?: boolean;
}

export interface HCRConfig {
  kernelBaseUrl: string;
  ipcToken: string;
  ownerOpenId: string | undefined;
}

// ── Command parsing ────────────────────────────────────────────────

// Matches "创建 Harness <id>：" or "创建 Harness <id>: " with Chinese/ASCII colon.
// harness_id must match ^[a-z0-9]+(?:-[a-z0-9]+)*$ — lowercase, digits, single hyphens only.
const HCR_RE = /^创建\s+Harness\s+([a-z0-9]+(?:-[a-z0-9]+)*)[：:]\s*(.+)$/;

export function parseHarnessChangeCommand(text: string): HCRCommand | null {
  const trimmed = text.trim();
  const m = trimmed.match(HCR_RE);
  if (!m) return null;
  return {
    harnessId: m[1],
    requirement: m[2].trim(),
  };
}

// ── Authorisation pre-filter ──────────────────────────────────────

export function isHarnessChangeAuthorised(
  config: HCRConfig,
  chatType: string,
  senderOpenId: string,
): string | null {
  if (!config.ownerOpenId) {
    return "Harness 创建功能未配置 (owner 未设置)";
  }
  if (chatType !== "p2p") {
    return "Harness 创建仅支持私聊";
  }
  if (senderOpenId !== config.ownerOpenId) {
    return "您不是 Harness 创建人";
  }
  return null;
}

// ── HCR support status ────────────────────────────────────────────

/** Known non-create commands that are explicitly unsupported. */
const UNSUPPORTED_RE = /^(修改|优化|删除|注册|启用)\s+Harness/;

export function isUnsupportedCommand(text: string): boolean {
  return UNSUPPORTED_RE.test(text.trim());
}

// ── Kernel API call ───────────────────────────────────────────────

export async function postHarnessChangeRequest(
  config: HCRConfig,
  harnessId: string,
  requirement: string,
  sourceMessageId: string,
  originalPayload: unknown,
): Promise<HCRResult> {
  const url = `${config.kernelBaseUrl}/v1/harness-change-requests`;
  const body = {
    harness_id: harnessId,
    requirement,
    source_message_id: sourceMessageId,
    payload: originalPayload,
  };

  const response = await fetch(url, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${config.ipcToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });

  const data = await response.json().catch(() => ({ error: "invalid_json_response" }));

  if (response.ok && data.ok) {
    const dedupText = data.deduplicated ? "（重复消息，未重复创建）" : "";
    return {
      ok: true,
      replyText: `已接收 Harness 创建请求。\n请求编号：${data.request_id}\n当前状态：等待开发执行。${dedupText}`,
      requestId: data.request_id,
      deduplicated: data.deduplicated,
    };
  }

  const errorMsg = String(data.error || "未知错误");

  if (errorMsg.startsWith("HARNESS_CHANGE_REQUEST_OWNER_REQUIRED")) {
    return { ok: false, replyText: "您不是配置的 Harness 创建人。" };
  }
  if (errorMsg.startsWith("HARNESS_CHANGE_REQUEST_P2P_REQUIRED")) {
    return { ok: false, replyText: "Harness 创建仅支持私聊。" };
  }
  if (errorMsg.startsWith("HARNESS_CHANGE_REQUEST_CHANNEL_REQUIRED")) {
    return { ok: false, replyText: "Harness 创建仅支持飞书渠道。" };
  }
  if (errorMsg.startsWith("INVALID_HARNESS_ID")) {
    return { ok: false, replyText: `无效的 Harness ID。Harness ID 必须是小写字母、数字和单个连字符。` };
  }
  if (errorMsg.startsWith("EMPTY_HARNESS_REQUIREMENT")) {
    return { ok: false, replyText: "需求描述不能为空。" };
  }
  if (errorMsg.startsWith("INVALID_SOURCE_MESSAGE_ID")) {
    return { ok: false, replyText: "消息 ID 缺失，无法创建 Harness。" };
  }

  return { ok: false, replyText: "Harness 创建失败，请稍后重试。" };
}
