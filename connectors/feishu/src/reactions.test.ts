import test from "node:test";
import assert from "node:assert/strict";
import { createReactionTracker, extractReactionId } from "./reactions.js";

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
  const tracker = createReactionTracker(config(), client);

  await tracker.markProcessing("om_1");
  await tracker.markSucceeded("om_1");

  assert.deepEqual(client.operations, [
    "POST:/open-apis/im/v1/messages/om_1/reactions:OK",
    "DELETE:/open-apis/im/v1/messages/om_1/reactions/reaction_1",
  ]);
});

test("reaction tracker replaces processing with failed marker", async () => {
  const client = new FakeReactionClient();
  const tracker = createReactionTracker(config(), client);

  await tracker.markProcessing("om_2");
  await tracker.markFailed("om_2");

  assert.deepEqual(client.operations, [
    "POST:/open-apis/im/v1/messages/om_2/reactions:OK",
    "DELETE:/open-apis/im/v1/messages/om_2/reactions/reaction_1",
    "POST:/open-apis/im/v1/messages/om_2/reactions:ERROR",
  ]);
});

function config() {
  return {
    appId: "app",
    appSecret: "secret",
    kernelUrl: "http://127.0.0.1:4130/v1/ingress",
    kernelIngressTimeoutMs: 100,
    connectorPort: 4131,
    ipcToken: "token",
    processingReactionEmoji: "OK",
    failedReactionEmoji: "ERROR",
  };
}

class FakeReactionClient {
  operations: string[] = [];
  nextId = 1;

  async request(input: any) {
    if (input.method === "POST") {
      this.operations.push(`${input.method}:${input.url}:${input.data.reaction_type.emoji_type}`);
      return { data: { reaction_id: `reaction_${this.nextId++}` } };
    }
    this.operations.push(`${input.method}:${input.url}`);
    return { data: {} };
  }
}
