/**
 * Durable chat-Run terminal observer.
 *
 * The Host exposes generic Journal facts; this Feishu edge owns reaction
 * cleanup and user-facing terminal wording. It consumes the unfiltered event
 * stream in sequence so new IngressAccepted/RunStarted correlations stay live.
 */

import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { ConnectorConfig } from "./config.js";
import type { ExecuteStore } from "./execute-store.js";
import type { ReactionTracker } from "./reactions.js";
import type { FeishuTransport } from "./transport.js";

export const FAILURE_HINT_TEXT = "任务已停止。\n原因：执行失败。\n\n你可以回复“继续”接着处理。";
export const TOOL_LIMIT_HINT_TEXT =
  "任务已停止。\n原因：本轮工具调用达到上限。\n\n你可以回复“继续”接着处理。";
export const CANCELLED_HINT_TEXT = "任务已停止。\n原因：你请求停止了本轮执行。";

const DEFAULT_POLL_INTERVAL_MS = 1000;
const DEFAULT_PAGE_LIMIT = 1000;

export interface RunFailureObserver {
  start(): void;
  stop(): void;
  tick(): Promise<void>;
}

type Fetcher = (url: string, init: RequestInit) => Promise<Response>;
export type RunFailureObserverDeps = {
  fetcher?: Fetcher;
  cursorPath?: string;
  pollIntervalMs?: number;
};

export function createRunFailureObserver(
  config: ConnectorConfig,
  transport: FeishuTransport,
  reactions: ReactionTracker,
  executeStore?: ExecuteStore,
  deps: RunFailureObserverDeps = {},
): RunFailureObserver {
  const fetcher: Fetcher = deps.fetcher ?? ((url, init) => fetch(url, init));
  const cursorPath = deps.cursorPath ?? config.runFailureCursorPath;
  const pollIntervalMs = deps.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS;
  const eventsBase = config.kernelUrl.replace(/\/v1\/ingress$/, "");
  const ingressByEventId = new Map<string, string>();
  const runIdToTrigger = new Map<string, string>();

  let cursor = loadCursor(cursorPath);
  let initialized = false;
  let initialization: Promise<void> | null = null;
  let activeTick: Promise<void> | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;

  async function fetchEvents(fromCursor: number) {
    const response = await fetcher(
      `${eventsBase}/v1/events?cursor=${fromCursor}&limit=${DEFAULT_PAGE_LIMIT}`,
      {
        method: "GET",
        headers: {
          authorization: `Bearer ${config.ipcToken}`,
          "content-type": "application/json",
        },
      },
    );
    if (!response.ok) throw new Error(`events fetch failed: HTTP ${response.status}`);
    const body = (await response.json()) as any;
    return {
      events: Array.isArray(body.events) ? body.events : [],
      nextCursor: Number(body.next_cursor ?? fromCursor),
      hasMore: Boolean(body.has_more),
    };
  }

  function remember(event: any) {
    if (event?.event_kind === "IngressAccepted") {
      const eventId = event?.payload?.event_id;
      const messageId = event?.payload?.message_id;
      if (typeof eventId === "string" && typeof messageId === "string") {
        ingressByEventId.set(eventId, messageId);
      }
    } else if (event?.event_kind === "RunStarted") {
      const runId = event?.payload?.run_id ?? event?.run_id;
      const trigger = event?.payload?.trigger_event_id ?? event?.correlation_id;
      if (typeof runId === "string" && typeof trigger === "string") {
        runIdToTrigger.set(runId, trigger);
      }
    }
  }

  async function deliverTerminal(event: any) {
    const runId = event?.payload?.run_id ?? event?.run_id;
    if (typeof runId !== "string" || !runId) return;
    const trigger = runIdToTrigger.get(runId);
    const messageId = trigger ? ingressByEventId.get(trigger) : undefined;
    if (!messageId) return; // non-chat or automatic continuation Run
    const idempotencyKey = `run-terminal:${runId}`;
    if (executeStore?.get(idempotencyKey)?.status === "sent") {
      await reactions.clearProcessing(messageId);
      return;
    }
    await reactions.clearProcessing(messageId);
    const receipt = await transport.replyToMessage(messageId, "text", {
      text: renderTerminal(event),
    });
    if (executeStore) {
      const now = new Date().toISOString();
      executeStore.set({
        idempotencyKey,
        invocationId: `terminal:${runId}`,
        operation: "feishu.run_terminal",
        status: "sent",
        receiptSummary: { messageId: receipt.message_id },
        createdAt: now,
        updatedAt: now,
      });
    }
    console.log(
      `run terminal delivered kind=${event.event_kind} run=${shortId(runId)} msg=${shortId(messageId)}`,
    );
  }

  async function consume(event: any, deliver: boolean) {
    remember(event);
    if (
      deliver &&
      (event?.event_kind === "RunFailed" || event?.event_kind === "RunCancelled")
    ) {
      await deliverTerminal(event);
    }
  }

  async function initialize() {
    const storedCursor = cursor;
    let scan = 0;
    let highWater = 0;
    for (;;) {
      const page = await fetchEvents(scan);
      for (const event of page.events) {
        // event.observe.v0 deliberately exposes a page cursor rather than a
        // per-event SQLite sequence. Rebuild correlation maps silently here;
        // replay after a stored cursor happens in pollNew below.
        await consume(event, false);
      }
      scan = page.nextCursor;
      highWater = Math.max(highWater, scan);
      if (!page.hasMore) break;
    }
    // First install establishes a baseline. A restart replays every event
    // after its durable cursor, including terminal events received while down.
    if (storedCursor === null) {
      cursor = highWater;
      persistCursor(cursorPath, highWater);
    }
    initialized = true;
    if (storedCursor !== null) {
      await pollNew();
    }
  }

  async function pollNew() {
    let next = cursor ?? 0;
    for (;;) {
      const page = await fetchEvents(next);
      for (const event of page.events) {
        await consume(event, true);
      }
      // Advance only after the entire page (including every terminal
      // delivery) succeeds. A failure retries the page on the next tick;
      // connector-local idempotency suppresses already-sent terminals.
      next = page.nextCursor;
      cursor = next;
      persistCursor(cursorPath, next);
      if (!page.hasMore) break;
    }
  }

  async function tickOnce() {
    if (!initialized) {
      if (!initialization) {
        initialization = initialize().catch((error) => {
          initialization = null;
          throw error;
        });
      }
      await initialization;
      return;
    }
    await pollNew();
  }

  function tick(): Promise<void> {
    if (activeTick) return activeTick;
    activeTick = tickOnce().finally(() => {
      activeTick = null;
    });
    return activeTick;
  }

  return {
    start() {
      if (timer) return;
      void tick().catch(logPollError);
      timer = setInterval(() => void tick().catch(logPollError), pollIntervalMs);
    },
    stop() {
      if (timer) clearInterval(timer);
      timer = null;
    },
    tick,
  };
}

export function renderTerminal(event: any): string {
  if (event?.event_kind === "RunCancelled") return CANCELLED_HINT_TEXT;
  const reason = String(event?.payload?.reason ?? event?.payload?.error_category ?? "");
  if (reason.includes("tool_call_limit_reached")) return TOOL_LIMIT_HINT_TEXT;
  if (reason.includes("context_over_budget")) {
    return "任务已停止。\n原因：本轮上下文超过模型限制。\n\n你可以发送 /reset 开启新会话。";
  }
  if (reason.includes("model")) {
    return "任务已停止。\n原因：模型调用失败。\n\n你可以回复“继续”重试。";
  }
  return FAILURE_HINT_TEXT;
}

function loadCursor(path: string): number | null {
  try {
    if (!existsSync(path)) return null;
    const value = Number(JSON.parse(readFileSync(path, "utf8"))?.cursor);
    return Number.isFinite(value) && value >= 0 ? value : null;
  } catch {
    return null;
  }
}

function persistCursor(path: string, cursor: number) {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.${process.pid}.tmp`;
  writeFileSync(
    temporary,
    JSON.stringify({ version: 1, cursor, updatedAt: new Date().toISOString() }),
  );
  renameSync(temporary, path);
}

function logPollError(error: any) {
  console.error(`run observer poll failed: ${String(error?.message || error).slice(0, 200)}`);
}

function shortId(value: string) {
  return value.length <= 10 ? value : `${value.slice(0, 4)}...${value.slice(-4)}`;
}
