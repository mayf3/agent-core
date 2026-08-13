import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  CANCELLED_HINT_TEXT,
  TOOL_LIMIT_HINT_TEXT,
  createRunFailureObserver,
} from "./run-observer.js";
import type { ReactionTracker } from "./reactions.js";
import type { FeishuTransport } from "./transport.js";

type FakeEvent = { sequence: number; event_kind: string; run_id?: string; payload: any };

function fakeJournal(events: FakeEvent[]) {
  return {
    events,
    fetcher: async (url: string) => {
      const cursor = Number(new URL(url).searchParams.get("cursor") ?? 0);
      const matches = events
        .filter((event) => event.sequence > cursor)
        .sort((a, b) => a.sequence - b.sequence);
      const page = matches.slice(0, 1000);
      return new Response(
        JSON.stringify({
          events: page,
          next_cursor: page.length ? page[page.length - 1].sequence : cursor,
          has_more: matches.length > page.length,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    },
  };
}

class FakeReactions implements ReactionTracker {
  cleared: string[] = [];
  failures = 0;
  async markProcessing() {}
  async markSucceeded() {}
  async markFailed() {}
  async clearProcessing(messageId: string) {
    if (this.failures > 0) {
      this.failures -= 1;
      throw new Error("temporary reaction removal failure");
    }
    this.cleared.push(messageId);
  }
}

class FakeTransport implements FeishuTransport {
  replies: Array<{ messageId: string; text: string }> = [];
  failures = 0;
  async replyToMessage(messageId: string, _type: string, content: any) {
    if (this.failures > 0) {
      this.failures -= 1;
      throw new Error("temporary send failure");
    }
    this.replies.push({ messageId, text: content.text });
    return { message_id: `reply-${messageId}`, status: "sent" };
  }
}

function config(cursorPath: string): any {
  return {
    kernelUrl: "http://127.0.0.1:4130/v1/ingress",
    ipcToken: "ipc",
    runFailureCursorPath: cursorPath,
  };
}

function ingress(sequence: number, eventId: string, messageId: string): FakeEvent {
  return {
    sequence,
    event_kind: "IngressAccepted",
    payload: { event_id: eventId, message_id: messageId },
  };
}

function started(sequence: number, runId: string, eventId: string): FakeEvent {
  return {
    sequence,
    event_kind: "RunStarted",
    run_id: runId,
    payload: { run_id: runId, trigger_event_id: eventId },
  };
}

test("first installation baselines history, then delivers a new failure", async () => {
  const dir = mkdtempSync(join(tmpdir(), "terminal-observer-"));
  try {
    const path = join(dir, "cursor.json");
    const journal = fakeJournal([
      ingress(1, "event_old", "om_old"),
      started(2, "run_old", "event_old"),
      { sequence: 3, event_kind: "RunFailed", run_id: "run_old", payload: { run_id: "run_old" } },
    ]);
    const transport = new FakeTransport();
    const reactions = new FakeReactions();
    const observer = createRunFailureObserver(
      config(path),
      transport,
      reactions,
      undefined,
      { fetcher: journal.fetcher, cursorPath: path },
    );
    await observer.tick();
    assert.equal(transport.replies.length, 0, "historical failure is not replayed on install");
    journal.events.push(ingress(4, "event_new", "om_new"));
    journal.events.push(started(5, "run_new", "event_new"));
    journal.events.push({
      sequence: 6,
      event_kind: "RunFailed",
      run_id: "run_new",
      payload: { run_id: "run_new", reason: "tool_call_limit_reached: exec" },
    });
    await observer.tick();
    assert.deepEqual(reactions.cleared, ["om_new"]);
    assert.equal(transport.replies[0].text, TOOL_LIMIT_HINT_TEXT);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("restart replays a terminal event that arrived after the durable cursor", async () => {
  const dir = mkdtempSync(join(tmpdir(), "terminal-observer-"));
  try {
    const path = join(dir, "cursor.json");
    const journal = fakeJournal([ingress(1, "event_1", "om_1"), started(2, "run_1", "event_1")]);
    const first = createRunFailureObserver(
      config(path),
      new FakeTransport(),
      new FakeReactions(),
      undefined,
      { fetcher: journal.fetcher, cursorPath: path },
    );
    await first.tick();
    journal.events.push({
      sequence: 3,
      event_kind: "RunCancelled",
      run_id: "run_1",
      payload: { run_id: "run_1", reason: "user_requested" },
    });
    const transport = new FakeTransport();
    const restarted = createRunFailureObserver(
      config(path),
      transport,
      new FakeReactions(),
      undefined,
      { fetcher: journal.fetcher, cursorPath: path },
    );
    await restarted.tick();
    assert.equal(transport.replies[0].text, CANCELLED_HINT_TEXT);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("failed terminal delivery does not advance the cursor past the event", async () => {
  const dir = mkdtempSync(join(tmpdir(), "terminal-observer-"));
  try {
    const path = join(dir, "cursor.json");
    const journal = fakeJournal([ingress(1, "event_1", "om_1"), started(2, "run_1", "event_1")]);
    const transport = new FakeTransport();
    const observer = createRunFailureObserver(
      config(path),
      transport,
      new FakeReactions(),
      undefined,
      { fetcher: journal.fetcher, cursorPath: path },
    );
    await observer.tick();
    journal.events.push({
      sequence: 3,
      event_kind: "RunFailed",
      run_id: "run_1",
      payload: { run_id: "run_1" },
    });
    transport.failures = 1;
    await assert.rejects(observer.tick(), /temporary send failure/);
    await observer.tick();
    assert.equal(transport.replies.length, 1, "the same terminal fact is retried");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("failed processing-reaction removal does not advance past the terminal fact", async () => {
  const dir = mkdtempSync(join(tmpdir(), "terminal-observer-"));
  try {
    const path = join(dir, "cursor.json");
    const journal = fakeJournal([ingress(1, "event_1", "om_1"), started(2, "run_1", "event_1")]);
    const transport = new FakeTransport();
    const reactions = new FakeReactions();
    const observer = createRunFailureObserver(
      config(path),
      transport,
      reactions,
      undefined,
      { fetcher: journal.fetcher, cursorPath: path },
    );
    await observer.tick();
    journal.events.push({
      sequence: 3,
      event_kind: "RunFailed",
      run_id: "run_1",
      payload: { run_id: "run_1" },
    });
    reactions.failures = 1;
    await assert.rejects(observer.tick(), /temporary reaction removal failure/);
    assert.equal(transport.replies.length, 0, "terminal reply waits until OK removal succeeds");
    await observer.tick();
    assert.deepEqual(reactions.cleared, ["om_1"]);
    assert.equal(transport.replies.length, 1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
