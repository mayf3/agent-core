import test from "node:test";
import assert from "node:assert/strict";
import { validateExecute } from "./execute-server.js";

/** A minimal valid execute payload. */
function validPayload(overrides: Record<string, unknown> = {}) {
  return {
    protocol_version: "v1",
    operation: "feishu.send_message",
    invocation_id: "inv_1",
    decision_id: "dec_1",
    idempotency_key: "idem_1",
    arguments: { message_id: "om_1", text: "hi" },
    ...overrides,
  };
}

test("validateExecute accepts a well-formed payload", () => {
  assert.doesNotThrow(() => validateExecute(validPayload()));
});

test("validateExecute rejects unsupported protocol version", () => {
  assert.throws(
    () => validateExecute(validPayload({ protocol_version: "v2" })),
    /unsupported protocol version/,
  );
});

test("validateExecute rejects an operation the connector cannot execute", () => {
  // The connector only knows feishu.send_message; defense in depth — the
  // kernel's catalog allowlist is the primary guard.
  assert.throws(
    () => validateExecute(validPayload({ operation: "shell.exec" })),
    /operation_not_allowed/,
  );
});

test("validateExecute rejects a payload missing required fields", () => {
  for (const field of ["invocation_id", "decision_id", "idempotency_key"]) {
    const payload = validPayload();
    delete payload[field];
    assert.throws(
      () => validateExecute(payload),
      /invalid execute payload/,
      `missing ${field} should reject`,
    );
  }
});

test("validateExecute rejects a payload missing message_id or text", () => {
  for (const arg of ["message_id", "text"]) {
    const payload = validPayload();
    delete (payload.arguments as Record<string, unknown>)[arg];
    assert.throws(
      () => validateExecute(payload),
      /invalid execute payload/,
      `missing arguments.${arg} should reject`,
    );
  }
});
