import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import assert from "node:assert/strict";
import { loadLocalEnv, parseEnvLine } from "../src/env.mjs";

test("parses quoted and unquoted env lines", () => {
  assert.deepEqual(parseEnvLine('AGENT_CORE_MODEL="model-a"'), {
    key: "AGENT_CORE_MODEL",
    value: "model-a",
  });
  assert.deepEqual(parseEnvLine("AGENT_CORE_FEISHU_REQUIRE_GROUP_MENTION=false"), {
    key: "AGENT_CORE_FEISHU_REQUIRE_GROUP_MENTION",
    value: "false",
  });
  assert.equal(parseEnvLine("# comment"), null);
});

test("loads local env without overriding existing variables", async () => {
  const dir = await mkdtemp(join(tmpdir(), "agent-core-env-"));
  try {
    await mkdir(join(dir, "nested"));
    await writeFile(join(dir, ".env"), 'AGENT_CORE_MODEL="from-file"\nEXISTING_VALUE=file\n');
    const env = { EXISTING_VALUE: "shell" };
    const result = loadLocalEnv({ cwd: dir, env });
    assert.equal(result.loaded, true);
    assert.equal(env.AGENT_CORE_MODEL, "from-file");
    assert.equal(env.EXISTING_VALUE, "shell");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
