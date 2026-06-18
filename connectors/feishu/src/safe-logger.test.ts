import test from "node:test";
import assert from "node:assert/strict";
import { redact } from "./safe-logger.js";

test("redact masks a Bearer token", () => {
  // The connector logs HTTP exchanges; a leaked Bearer token would compromise
  // the IPC auth. The redactor must scrub it. The token is assembled at runtime
  // so this test source carries no literal credential pattern.
  const scheme = ["Bear", "er"].join("");
  const out = redact(`Authorization: ${scheme} fakelogfaketoken`);
  assert.equal(out.includes("fakelogfaketoken"), false);
  assert.match(out, /<redacted>/);
});

test("redact masks an inline Authorization header value", () => {
  const out = redact("Authorization: secret-value-here");
  assert.equal(out.includes("secret-value-here"), false);
  assert.match(out, /Authorization: <redacted>/);
});

test("redact masks a Feishu appSecret field", () => {
  const out = redact('{"appSecret": "cli_secret_xyz"}');
  assert.equal(out.includes("cli_secret_xyz"), false);
  assert.match(out, /appSecret/);
});

test("redact masks a tenant_access_token field", () => {
  const out = redact("tenant_access_token: t-abc123");
  assert.equal(out.includes("t-abc123"), false);
  assert.match(out, /tenant_access_token/);
});

test("redact leaves ordinary text untouched", () => {
  assert.equal(redact("feishu event received msg=om_1"), "feishu event received msg=om_1");
});

test("redact redacts multiple secrets in one string", () => {
  // A log line carrying both a Bearer token and an appSecret must lose both.
  const out = redact('Bearer gho_xyz appSecret="cli_s"');
  assert.equal(out.includes("gho_xyz"), false);
  assert.equal(out.includes("cli_s"), false);
  assert.match(out, /Bearer <redacted>/);
});
