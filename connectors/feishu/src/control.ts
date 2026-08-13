/** Feishu chat rescue commands. These never enter normal ingress or a model. */

import type { ConnectorConfig } from "./config.js";
import type { FeishuTransport } from "./transport.js";

export type ChatControlCommand = "status" | "stop" | "reset";

export function parseChatControl(text: string): ChatControlCommand | null {
  switch (text.trim()) {
    case "/status":
      return "status";
    case "/stop":
      return "stop";
    case "/reset":
      return "reset";
    default:
      return null;
  }
}

export async function tryHandleChatControl(
  config: ConnectorConfig,
  payload: any,
  transport: FeishuTransport,
  fetcher: typeof fetch = fetch,
): Promise<boolean> {
  if (payload?.message_type !== "text") return false;
  const command = parseChatControl(String(payload?.text ?? ""));
  if (!command) return false;
  const conversationKey = conversationKeyFor(payload);
  const requesterId = `feishu:open_id:${String(payload.sender_open_id || "")}`;
  const base = config.kernelUrl.replace(/\/v1\/ingress$/, "");
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), config.kernelIngressTimeoutMs);
  let reply: string;
  try {
    const response = await fetcher(`${base}/v1/chat-control`, {
      method: "POST",
      signal: controller.signal,
      headers: {
        authorization: `Bearer ${config.ipcToken}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        protocol_version: "chat-control.v0",
        action: command,
        channel: "Feishu",
        conversation_key: conversationKey,
        requester_id: requesterId,
      }),
    });
    const body = (await response.json().catch(() => ({}))) as any;
    if (!response.ok || body?.ok !== true) {
      throw new Error(`chat_control_http_${response.status}`);
    }
    reply = renderChatControl(command, body);
  } catch (error: any) {
    console.error(
      `chat control failed action=${command} error=${String(error?.message || error).slice(0, 120)}`,
    );
    reply = "控制命令暂时不可用，请稍后重试。";
  } finally {
    clearTimeout(timer);
  }
  await transport.replyToMessage(payload.message_id, "text", { text: reply });
  return true;
}

export function renderChatControl(command: ChatControlCommand, body: any): string {
  if (command === "reset") {
    return "已开启新会话，旧记录仍保留。后续消息会使用新 Session。";
  }
  if (command === "stop") {
    if (body.state === "cancellation_requested" || body.state === "already_requested") {
      return `已请求停止当前任务${body.run_id ? `（${body.run_id}）` : ""}。任务会在当前 model/tool 边界进入 Cancelled。`;
    }
    if (body.state === "already_cancelled") return "当前任务已经停止。";
    return "当前没有正在运行、可停止的任务。";
  }
  const run = body?.run;
  if (!run) {
    return body?.session_id
      ? `当前会话：${body.session_id}\n状态：Idle\n当前没有 Run。`
      : "状态：Idle\n当前还没有会话或 Run。";
  }
  const status = displayStatus(String(run.status || "Unknown"));
  const rows = [
    `当前任务：${String(run.task || "未知")}`,
    `Run：${String(run.run_id || "未知")}`,
    `状态：${status}`,
    `已运行：${formatDuration(Number(run.elapsed_ms || 0))}`,
    `工具调用：${Number(run.tool_calls_used || 0)} / ${run.tool_calls_max ?? "?"}`,
    `最近活动：${renderActivity(run.recent_activity)}`,
    `代码修改：${run.code_modified ? "有" : "无/未记录"}`,
  ];
  if (run.failure_reason) rows.push(`原因：${displayReason(String(run.failure_reason))}`);
  return rows.join("\n");
}

function conversationKeyFor(payload: any): string {
  if (payload.chat_type === "p2p") {
    return `feishu:open_id:${String(payload.sender_open_id || "")}`;
  }
  return `feishu:chat_id:${String(payload.chat_id || "")}`;
}

function displayStatus(status: string): string {
  const values: Record<string, string> = {
    Running: "Running",
    CancellationRequested: "Stopping",
    Cancelled: "Cancelled",
    WaitingDispatch: "Delivering",
    Completed: "Completed",
    Failed: "Failed",
    AwaitingApproval: "AwaitingApproval",
    Unknown: "Unknown",
  };
  return values[status] ?? status;
}

function displayReason(reason: string): string {
  if (reason.includes("tool_call_limit_reached")) return "tool_call_limit_reached";
  if (reason.includes("context_over_budget")) return "context_overflow";
  if (reason.includes("model")) return "model_failure";
  if (reason === "user_requested") return "user_requested";
  return reason.slice(0, 160);
}

function renderActivity(activity: any): string {
  if (!activity?.kind) return "暂无 durable activity";
  return activity.detail ? `${activity.kind} (${activity.detail})` : String(activity.kind);
}

function formatDuration(milliseconds: number): string {
  const total = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) return `${hours}h${minutes}m${seconds}s`;
  if (minutes > 0) return `${minutes}m${seconds}s`;
  return `${seconds}s`;
}
