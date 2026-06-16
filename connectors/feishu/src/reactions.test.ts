import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMemoryReactionStore } from "./reaction-store.js";
import { createReactionTracker, extractReactionId, withRetry } from "./reactions.js";

test("extractReactionId reads Feishu reaction responses", () => {
  assert.equal(
    extractReactionId({
      data: {
        reaction_id: "reaction_123",
      },
    }),
    "reaction_123",
  );
});

test("extractReactionId handles missing response data", () => {
  assert.equal(extractReactionId({}), "");
});

test("reaction tracker adds processing and removes it after success", async () => {
  const client = new FakeReactionClient();
  const tracker = createReactionTracker(config(), client, createMemoryReactionStore());

  await tracker.markProcessing("om_1");
  await tracker.markSucceeded("om_1");

  assert.deepEqual(client.operations, [
    "POST:/open-apis/im/v1/messages/om_1/reactions:OK",
    "DELETE:/open-apis/im/v1/messages/om_1/reactions/reaction_1",
  ]);
});

test("reaction tracker replaces processing with failed marker", async () => {
  const client = new FakeReactionClient();
  const tracker = createReactionTracker(config(), client, createMemoryReactionStore());

  await tracker.markProcessing("om_2");
  await tracker.markFailed("om_2");

  assert.deepEqual(client.operations, [
    "POST:/open-apis/im/v1/messages/om_2/reactions:OK",
    "DELETE:/open-apis/im/v1/messages/om_2/reactions/reaction_1",
    "POST:/open-apis/im/v1/messages/om_2/reactions:ERROR",
  ]);
});

test("reaction tracker deletes persisted processing state after restart", async () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-core-reactions-"));
  try {
    const statePath = join(dir, "reactions.jsonl");
    const firstClient = new FakeReactionClient();
    const firstTracker = createReactionTracker(config(statePath), firstClient);

    await firstTracker.markProcessing("om_3");

    const secondClient = new FakeReactionClient();
    const secondTracker = createReactionTracker(config(statePath), secondClient);
    await secondTracker.markSucceeded("om_3");

    assert.deepEqual(secondClient.operations, [
      "DELETE:/open-apis/im/v1/messages/om_3/reactions/reaction_1",
    ]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("withRetry succeeds on a transient failure", async () => {
  const op = failTimes(2, "ok");
  const result = await withRetry(op, 3, 0, "transient", noSleep);
  assert.equal(result, "ok");
});

test("withRetry exhausts attempts on a permanent failure and rethrows", async () => {
  const op = async () => {
    throw new Error("permanent failure");
  };
  await assert.rejects(
    withRetry(op, 3, 0, "permanent", noSleep),
    /permanent failure/,
  );
});

test("withRetry caps attempts at the configured bound (not a keepalive loop)", async () => {
  let calls = 0;
  const op = async () => {
    calls += 1;
    throw new Error("always fails");
  };
  await assert.rejects(withRetry(op, 4, 0, "cap", noSleep));
  assert.equal(calls, 4, "must call exactly attempts times, no more");
});

test("withRetry does not call the operation when attempts is 0 (floor at 1)", async () => {
  let calls = 0;
  await assert.rejects(
    withRetry(
      async () => {
        calls += 1;
        throw new Error("x");
      },
      0,
      0,
      "floor",
      noSleep,
    ),
  );
  assert.equal(calls, 1, "attempts=0 floors to 1 so the op runs at least once");
});

test("reaction tracker retries a transient processing-reaction add", async () => {
  const client = new FakeReactionClient({ failAddFor: 2 });
  const tracker = createReactionTracker(config(), client, createMemoryReactionStore());

  await tracker.markProcessing("om_4");

  assert.equal(client.addAttempts, 3, "two transient failures then success on the 3rd");
  assert.deepEqual(client.operations, [
    "POST:/open-apis/im/v1/messages/om_4/reactions:OK",
  ]);
});

test("reaction tracker gives up after exhausting add retries and does not record state", async () => {
  const store = createMemoryReactionStore();
  const client = new FakeReactionClient({ failAddFor: Infinity });
  const tracker = createReactionTracker(config(), client, store);

  await tracker.markProcessing("om_5");

  assert.equal(client.addAttempts, 3, "must stop after 3 attempts (bounded, not keepalive)");
  assert.equal(store.load().size, 0, "no state recorded when add never succeeded");
});

test("reaction tracker retries remove then succeeds", async () => {
  const client = new FakeReactionClient({ failDeleteFor: 1 });
  const tracker = createReactionTracker(config(), client, createMemoryReactionStore());

  await tracker.markProcessing("om_6");
  await tracker.markSucceeded("om_6");

  assert.equal(client.deleteAttempts, 2, "one transient failure then success");
  assert.equal(client.operations.length, 2);
});

function config(reactionStatePath = join(tmpdir(), "agent-core-reactions.jsonl")) {
  return {
    appId: "app",
    appSecret: "secret",
    kernelUrl: "http://127.0.0.1:4130/v1/ingress",
    kernelIngressTimeoutMs: 100,
    connectorPort: 4131,
    ipcToken: "token",
    processingReactionEmoji: "OK",
    failedReactionEmoji: "ERROR",
    reactionStatePath,
    reactionRetryAttempts: 3,
    reactionRetryBaseDelayMs: 0,
  };
}

function noSleep(): Promise<void> {
  return Promise.resolve();
}

/** Build an op that fails the first `failCount` calls then returns `successValue`. */
function failTimes<T>(failCount: number, successValue: T): () => Promise<T> {
  let calls = 0;
  return async () => {
    calls += 1;
    if (calls <= failCount) {
      throw new Error(`transient ${calls}`);
    }
    return successValue;
  };
}

class FakeReactionClient {
  operations: string[] = [];
  nextId = 1;
  addAttempts = 0;
  deleteAttempts = 0;
  private failAddFor: number;
  private failDeleteFor: number;

  constructor(options: { failAddFor?: number; failDeleteFor?: number } = {}) {
    this.failAddFor = options.failAddFor ?? 0;
    this.failDeleteFor = options.failDeleteFor ?? 0;
  }

  async request(input: any) {
    if (input.method === "POST") {
      this.addAttempts += 1;
      if (this.addAttempts <= this.failAddFor) {
        throw new Error(`add transient ${this.addAttempts}`);
      }
      this.operations.push(`${input.method}:${input.url}:${input.data.reaction_type.emoji_type}`);
      return { data: { reaction_id: `reaction_${this.nextId++}` } };
    }
    this.deleteAttempts += 1;
    if (this.deleteAttempts <= this.failDeleteFor) {
      throw new Error(`delete transient ${this.deleteAttempts}`);
    }
    this.operations.push(`${input.method}:${input.url}`);
    return { data: {} };
  }
}
