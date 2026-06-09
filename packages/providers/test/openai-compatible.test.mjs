import assert from "node:assert/strict";
import test from "node:test";
import { createOpenAiCompatibleProvider } from "../src/index.mjs";

test("provider reports missing config without network", async () => {
  const provider = createOpenAiCompatibleProvider({ env: {} });
  const result = await provider.generate({ messages: [] });

  assert.equal(result.ok, false);
  assert.equal(result.error.code, "model_config_required");
});

test("provider normalizes OpenAI-compatible tool calls", async () => {
  const provider = createOpenAiCompatibleProvider({
    apiKey: "test-key",
    model: "test-model",
    fetchImpl: async () => ({
      ok: true,
      json: async () => ({
        model: "test-model",
        choices: [{
          message: {
            content: "",
            tool_calls: [{
              id: "call_1",
              function: { name: "fs.read", arguments: "{\"path\":\"README.md\"}" },
            }],
          },
        }],
        usage: { prompt_tokens: 1, completion_tokens: 2, total_tokens: 3 },
      }),
    }),
  });
  const result = await provider.generate({ messages: [], tools: [{ name: "fs.read", description: "read" }] });

  assert.equal(result.ok, true);
  assert.equal(result.toolCalls[0].name, "fs.read");
  assert.equal(result.toolCalls[0].args.path, "README.md");
  assert.equal(result.usage.totalTokens, 3);
});
