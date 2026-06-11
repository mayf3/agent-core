import test from "node:test";
import assert from "node:assert/strict";
import { extractReactionId } from "./reactions.js";

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
