import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createRunFailureObserver,
  FAILURE_HINT_TEXT,
  type RunFailureObserverDeps,
} from "./run-observer.js";
import type { ReactionTracker } from "./reactions.js";
import type { FeishuTransport } from "./transport.js";

type FakeEvent = {
  sequence: number;
  event_kind: string;
  payload: any;
};

/** In-memory journal: pages events by kind + cursor, like /v1/events. */
function fakeJournal() {
  let nextSeq = 1;
  const events: FakeEvent[] = [];
  const push = (event_kind: string, payload: any) => {
    events.push({ sequence: nextSeq++, event_kind, payload });
  };
  return {
    events,
    push,
    fetcher: async (url: string) => {
      const parsed = new URL(url);
      const cursor = Number(parsed.searchParams.get("cursor") ?? 0);
      const kind = parsed.searchParams.get("event_kind") ?? "";
      const limit = Number(parsed.searchParams.get("limit") ?? 1000);
      const matches = events.filter(
        (e) => (kind === "" || e.event_kind === kind) && e.sequence > cursor,
      );
      const page = matches.slice(0, limit);
      return {
        ok: true,
        async json() {
          return {
            events: page,
            next_cursor: page.length ? page[page.length - 1].sequence : cursor,
            has_more: matches.length > limit,
          };
        },
      } as Response;
    },
  };
}

class FakeReactionTracker implements ReactionTracker {
  cleared: string[] = [];
  async markProcessing(): Promise<void> {}
  async markSucceeded(): Promise<void> {}
  async markFailed(): Promise<void> {}
  async clearProcessing(messageId: string): Promise<void> {
    this.cleared.push(messageId);
  }
}

class FakeTransport implements FeishuTransport {
  replies: { messageId: string; text: string }[] = [];
  async replyToMessage(messageId: string, msgType: string, content: unknown): Promise<any> {
    this.replies.push({ messageId, text: (content as any)?.text ?? "" });
    return { message_id: `reply-${messageId}`, status: "ok" };
  }
}

function config(cursorPath: string) {
  return {
    appId: "app",
    appSecret: "secret",
    kernelUrl: "http://127.0.0.1:4130/v1/ingress",
    kernelIngressTimeoutMs: 100,
    connectorPort: 4131,
    ipcToken: "token",
    processingReactionEmoji: "OK",
    failedReactionEmoji: "ERROR",
    reactionStatePath: join(tmpdir(), "agent-core-reactions.jsonl"),
    reactionRetryAttempts: 1,
    reactionRetryBaseDelayMs: 0,
    executeStatePath: join(tmpdir(), "agent-core-executes.jsonl"),
    runFailureCursorPath: cursorPath,
    kernelDecisionApiUrl: "http://127.0.0.1:4130",
    kernelDecisionToken: "decision-token",
    feishuOwnerOpenId: "ou_owner",
  };
}

function deps(journal: ReturnType<typeof fakeJournal>, cursorPath: string): RunFailureObserverDeps {
  return { fetcher: journal.fetcher, cursorPath, pollIntervalMs: 10_000 };
}

/** Push an ingress -> runstarted -> runfailed chain for one run. */
function pushFailedRun(journal: ReturnType<typeof fakeJournal>, runId: string, messageId: string) {
  const triggerId = `ev_ingress_${runId}`;
  journal.push("IngressAccepted", { event_id: triggerId, message_id: messageId });
  journal.push("RunStarted", { run_id: runId, trigger_event_id: triggerId });
  journal.push("RunFailed", { run_id: runId, error_category: "runtime_v0_failed" });
}

test("historical RunFailed events are never re-fired (first-run catch-up)", async () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-core-observer-"));
  try {
    const cursorPath = join(dir, "cursor.json");
    const journal = fakeJournal();
    pushFailedRun(journal, "run_old", "om_old");
    const reactions = new FakeReactionTracker();
    const transport = new FakeTransport();
    const observer = createRunFailureObserver(config(cursorPath), transport, reactions, deps(journal, cursorPath));

    await observer.tick();

    assert.deepEqual(reactions.cleared, [], "historical failure must not fire");
    assert.deepEqual(transport.replies, [], "historical failure must not reply");
    observer.stop();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a failure after startup clears the OK and sends the failure hint", async () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-core-observer-"));
  try {
    const cursorPath = join(dir, "cursor.json");
    const journal = fakeJournal();
    const reactions = new FakeReactionTracker();
    const transport = new FakeTransport();
    const observer = createRunFailureObserver(config(cursorPath), transport, reactions, deps(journal, cursorPath));

    await observer.tick(); // startup (empty journal)
    pushFailedRun(journal, "run_2", "om_msg_2");
    await observer.tick();

    assert.deepEqual(reactions.cleared, ["om_msg_2"]);
    assert.equal(transport.replies.length, 1);
    assert.equal(transport.replies[0].messageId, "om_msg_2");
    assert.equal(transport.replies[0].text, FAILURE_HINT_TEXT);
    observer.stop();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the same run failure fires exactly once", async () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-core-observer-"));
  try {
    const cursorPath = join(dir, "cursor.json");
    const journal = fakeJournal();
    const reactions = new FakeReactionTracker();
    const transport = new FakeTransport();
    const observer = createRunFailureObserver(config(cursorPath), transport, reactions, deps(journal, cursorPath));

    await observer.tick();
    pushFailedRun(journal, "run_3", "om_msg_3");
    journal.push("RunFailed", { run_id: "run_3" }); // duplicate
    await observer.tick();

    assert.deepEqual(reactions.cleared, ["om_msg_3"]);
    assert.equal(transport.replies.length, 1);
    observer.stop();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("failure without a correlated ingress is skipped", async () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-core-observer-"));
  try {
    const cursorPath = join(dir, "cursor.json");
    const journal = fakeJournal();
    const reactions = new FakeReactionTracker();
    const transport = new FakeTransport();
    const observer = createRunFailureObserver(config(cursorPath), transport, reactions, deps(journal, cursorPath));

    await observer.tick();
    journal.push("RunStarted", { run_id: "run_4", trigger_event_id: "ev_unknown" });
    journal.push("RunFailed", { run_id: "run_4" });
    await observer.tick();

    assert.deepEqual(reactions.cleared, []);
    assert.deepEqual(transport.replies, []);
    observer.stop();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("cursor is persisted and a restarted observer resumes without re-firing", async () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-core-observer-"));
  try {
    const cursorPath = join(dir, "cursor.json");
    const journal = fakeJournal();
    pushFailedRun(journal, "run_5", "om_msg_5");

    const first = createRunFailureObserver(
      config(cursorPath),
      new FakeTransport(),
      new FakeReactionTracker(),
      deps(journal, cursorPath),
    );
    await first.tick(); // first run: absorbs run_5, persists cursor
    first.stop();

    // Connector was down; a new failure arrives while it is away.
    pushFailedRun(journal, "run_6", "om_msg_6");

    const reactions = new FakeReactionTracker();
    const transport = new FakeTransport();
    const second = createRunFailureObserver(config(cursorPath), transport, reactions, deps(journal, cursorPath));
    await second.tick();

    // Only run_6 fires (run_5 was absorbed before the persisted cursor).
    assert.deepEqual(reactions.cleared, ["om_msg_6"]);
    assert.equal(transport.replies.length, 1);
    assert.equal(transport.replies[0].messageId, "om_msg_6");
    second.stop();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
