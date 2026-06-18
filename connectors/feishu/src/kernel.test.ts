import test from "node:test";
import assert from "node:assert/strict";
import { normalizeMessageEvent } from "./kernel.js";

/** A canonical Feishu v2 message event payload. */
function feishuEvent(overrides: Record<string, unknown> = {}) {
  return {
    header: { event_id: "evt_1" },
    event: {
      message: {
        message_id: "om_1",
        chat_id: "oc_1",
        chat_type: "p2p",
        message_type: "text",
        content: JSON.stringify({ text: "你好" }),
        mentions: [],
      },
      sender: { sender_id: { open_id: "ou_user" }, sender_type: "user" },
    },
    ...overrides,
  };
}

test("normalizeMessageEvent derives the dedupe key as message:<messageId>", () => {
  // The kernel dedupes ingress by external_event_id; the connector MUST derive
  // it as "message:<messageId>" so a re-delivered Feishu event is idempotent
  // (see docs/decisions/connector-local-durability.md).
  const normalized = normalizeMessageEvent(feishuEvent());
  assert.equal(normalized.external_event_id, "message:om_1");
  assert.equal(normalized.payload.message_id, "om_1");
});

test("normalizeMessageEvent falls back to the provider event id without a message_id", () => {
  // An event without a message_id cannot dedupe by message; fall back to the
  // provider event id rather than a placeholder collision.
  const raw = feishuEvent();
  delete (raw.event as any).message.message_id;
  const normalized = normalizeMessageEvent(raw);
  assert.equal(normalized.external_event_id, "evt_1");
});

test("normalizeMessageEvent parses a JSON-string content into text", () => {
  const normalized = normalizeMessageEvent(feishuEvent());
  assert.equal(normalized.payload.text, "你好");
  assert.equal(normalized.payload.message_type, "text");
});

test("normalizeMessageEvent treats a plain-string content as text", () => {
  const raw = feishuEvent();
  (raw.event as any).message.content = "hello";
  const normalized = normalizeMessageEvent(raw);
  assert.equal(normalized.payload.text, "hello");
});

test("normalizeMessageEvent normalizes mentions into {open_id, name}", () => {
  const raw = feishuEvent({
    event: {
      message: {
        message_id: "om_2",
        chat_id: "oc_1",
        chat_type: "group",
        message_type: "text",
        content: JSON.stringify({ text: "@bot hi" }),
        mentions: [{ id: { open_id: "ou_bot", name: "Bot" } }, { open_id: "ou_other" }],
      },
      sender: { sender_id: { open_id: "ou_user" }, sender_type: "user" },
    },
  });
  const normalized = normalizeMessageEvent(raw);
  assert.deepEqual(normalized.payload.mentions, [
    { open_id: "ou_bot", name: "Bot" },
    { open_id: "ou_other", name: "" },
  ]);
  assert.equal(normalized.payload.chat_type, "group");
});
