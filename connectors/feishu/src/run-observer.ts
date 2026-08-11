/**
 * Run terminal failure observer (FEISHU_FAILURE_FEEDBACK_V0).
 *
 * Boundary: the Kernel/Runtime only ever produces the generic `RunFailed`
 * fact (run_id + reason/category) in its journal. This connector-side
 * observer polls the Kernel's read-only event-observe endpoint
 * (`GET /v1/events`, `event.observe.v0`), correlates a failed Run back to
 * the Feishu message that started it, and performs the channel-specific
 * cleanup: remove the processing OK reaction and send a safe failure hint.
 *
 * No Kernel/Runtime code is involved and no Feishu display semantics live
 * in Core. The correlation chain is:
 *
 *   RunFailed{run_id}
 *     → RunStarted{trigger_event_id} (payload)
 *     → IngressAccepted{event_id, message_id} (payload)
 *     → clearProcessing(message_id) + safe failure reply
 *
 * The observer polls ALL journal events with a single cursor so the
 * correlation maps stay current for runs that start after startup, and it
 * fires exactly once per failed Run (handledRunIds + persisted cursor).
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { ConnectorConfig } from "./config.js";
import type { ReactionTracker } from "./reactions.js";
import type { FeishuTransport } from "./transport.js";

/** Safe, channel-neutral failure hint. No internal error categories. */
export const FAILURE_HINT_TEXT = "刚才的任务已经执行失败并停止，处理中状态已清除。";

const DEFAULT_POLL_INTERVAL_MS = 3000;
const DEFAULT_PAGE_LIMIT = 1000;

export interface RunFailureObserver {
  /** Start the polling loop (idempotent). */
  start(): void;
  /** Stop the polling loop. */
  stop(): void;
  /** Poll once: fetch one page of events and process it. */
  tick(): Promise<void>;
}

type Fetcher = (url: string, init: RequestInit) => Promise<Response>;

/** Dependency injection surface for tests. */
export type RunFailureObserverDeps = {
  fetcher?: Fetcher;
  cursorPath?: string;
  pollIntervalMs?: number;
};

export function createRunFailureObserver(
  config: ConnectorConfig,
  transport: FeishuTransport,
  reactions: ReactionTracker,
  deps: RunFailureObserverDeps = {},
): RunFailureObserver {
  const fetcher: Fetcher = deps.fetcher ?? ((url, init) => fetch(url, init));
  const cursorPath = deps.cursorPath ?? config.runFailureCursorPath;
  const pollIntervalMs = deps.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS;
  const eventsBase = config.kernelUrl.replace(/\/v1\/ingress$/, "");

  // Correlation maps, maintained incrementally from journal events.
  const ingressByEventId = new Map<string, string>();
  const runIdToTrigger = new Map<string, string>();
  const handledRunIds = new Set<string>();

  let cursor = 0;
  let initialized: Promise<void> | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;

  function authHeaders(): Record<string, string> {
    return {
      authorization: `Bearer ${config.ipcToken}`,
      "content-type": "application/json",
    };
  }

  async function fetchEvents(
    fromCursor: number,
  ): Promise<{ events: any[]; nextCursor: number; hasMore: boolean }> {
    const url = `${eventsBase}/v1/events?cursor=${fromCursor}&limit=${DEFAULT_PAGE_LIMIT}`;
    const res = await fetcher(url, { method: "GET", headers: authHeaders() });
    if (!res.ok) {
      throw new Error(`events fetch failed: HTTP ${res.status}`);
    }
    const body = (await res.json()) as {
      events?: any[];
      next_cursor?: number;
      has_more?: boolean;
    };
    return {
      events: body.events ?? [],
      nextCursor: Number(body.next_cursor ?? fromCursor),
      hasMore: Boolean(body.has_more),
    };
  }

  /** Maintain the correlation maps from a single observed event. */
  function updateMaps(ev: any): void {
    if (ev?.event_kind === "IngressAccepted") {
      const messageId = ev?.payload?.message_id;
      const eventId = ev?.payload?.event_id;
      if (typeof messageId === "string" && typeof eventId === "string") {
        ingressByEventId.set(eventId, messageId);
      }
    } else if (ev?.event_kind === "RunStarted") {
      const runId = ev?.payload?.run_id;
      const triggerId = ev?.payload?.trigger_event_id;
      if (typeof runId === "string" && typeof triggerId === "string") {
        runIdToTrigger.set(runId, triggerId);
      }
    }
  }

  /**
   * Startup initialization:
   * 1. Scan journal history (silent — maps only). On the very first run the
   *    historical RunFailed runs are also absorbed as already-handled and
   *    the resume cursor is persisted at the journal end.
   * 2. Later restarts resume from the persisted cursor; runs that failed
   *    while the connector was away still fire.
   */
  async function initialize(): Promise<void> {
    const state = loadCursorState(cursorPath);
    const firstRun = state === null;
    let c = 0;
    for (;;) {
      const page = await fetchEvents(c);
      for (const ev of page.events) {
        updateMaps(ev);
        if (firstRun && ev?.event_kind === "RunFailed") {
          const runId = ev?.payload?.run_id;
          if (typeof runId === "string") {
            handledRunIds.add(runId);
          }
        }
      }
      c = page.nextCursor;
      if (!page.hasMore) break;
    }
    if (firstRun) {
      cursor = c;
      persistCursorState(cursorPath, cursor);
    } else {
      cursor = state.cursor;
    }
  }

  async function handleRunFailed(runId: string): Promise<void> {
    if (handledRunIds.has(runId)) {
      return;
    }
    handledRunIds.add(runId); // mark before awaiting so ticks never double-fire
    const triggerId = runIdToTrigger.get(runId);
    if (!triggerId) {
      console.log(`run failure skipped run=${runId.slice(0, 8)} reason=no_runstarted`);
      return;
    }
    const messageId = ingressByEventId.get(triggerId);
    if (!messageId) {
      console.log(`run failure skipped run=${runId.slice(0, 8)} reason=no_ingress`);
      return;
    }
    await reactions.clearProcessing(messageId);
    try {
      await transport.replyToMessage(messageId, "text", { text: FAILURE_HINT_TEXT });
      console.log(`failure feedback sent run=${runId.slice(0, 8)} msg=${messageId.slice(-4)}`);
    } catch (error: any) {
      console.error(
        `failure reply failed run=${runId.slice(0, 8)} msg=${messageId.slice(-4)} error=${(error?.message || String(error)).slice(0, 200)}`,
      );
    }
  }

  async function pollOnce(): Promise<void> {
    if (!initialized) {
      initialized = initialize();
    }
    await initialized;
    for (;;) {
      const page = await fetchEvents(cursor);
      for (const ev of page.events) {
        updateMaps(ev);
        if (ev?.event_kind === "RunFailed") {
          const runId = ev?.payload?.run_id;
          if (typeof runId === "string") {
            await handleRunFailed(runId);
          }
        }
      }
      cursor = page.nextCursor;
      persistCursorState(cursorPath, cursor);
      if (!page.hasMore) break;
    }
  }

  return {
    start() {
      if (timer) {
        return;
      }
      void pollOnce().catch((error) => {
        console.error(`run observer poll failed: ${(error?.message || String(error)).slice(0, 200)}`);
      });
      timer = setInterval(() => {
        void pollOnce().catch((error) => {
          console.error(`run observer poll failed: ${(error?.message || String(error)).slice(0, 200)}`);
        });
      }, pollIntervalMs);
    },
    stop() {
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
    },
    async tick() {
      await pollOnce();
    },
  };
}

function loadCursorState(path: string): { cursor: number } | null {
  try {
    if (!existsSync(path)) {
      return null;
    }
    const parsed = JSON.parse(readFileSync(path, "utf8"));
    const value = Number(parsed?.cursor ?? 0);
    return { cursor: Number.isFinite(value) && value > 0 ? value : 0 };
  } catch {
    return null;
  }
}

function persistCursorState(path: string, cursor: number): void {
  try {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, JSON.stringify({ version: 1, cursor, updatedAt: new Date().toISOString() }));
  } catch (error: any) {
    console.error(`run observer cursor persist failed: ${(error?.message || String(error)).slice(0, 200)}`);
  }
}
