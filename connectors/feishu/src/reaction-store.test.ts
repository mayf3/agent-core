import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createJsonlReactionStore } from "./reaction-store.js";

test("jsonl reaction store compacts to active states", () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-core-reaction-store-"));
  try {
    const filePath = join(dir, "reactions.jsonl");
    const store = createJsonlReactionStore(filePath, { compactAfterBytes: 1 });

    store.set(state("om_1", "reaction_1"));
    store.set(state("om_2", "reaction_2"));
    store.delete("om_1");

    assert.deepEqual([...store.load().keys()], ["om_2"]);
    const text = readFileSync(filePath, "utf8");
    assert.match(text, /"om_2"/);
    assert.doesNotMatch(text, /"om_1"/);
    assert.doesNotMatch(text, /"op":"delete"/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

function state(messageId: string, reactionId: string) {
  return {
    messageId,
    reactionId,
    emojiType: "OK",
    status: "processing" as const,
    updatedAt: new Date().toISOString(),
  };
}
