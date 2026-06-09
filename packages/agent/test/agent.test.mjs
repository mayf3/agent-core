import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { readApprovals, readEvents, readStateRecords } from "../../core/src/index.mjs";
import { runAgentTurn } from "../src/index.mjs";

test("agent returns a final answer without tools", async () => {
  const env = await tempEnv();
  try {
    const provider = fakeProvider([{ ok: true, text: "hello", toolCalls: [] }]);
    const result = await runAgentTurn({ text: "hi", provider, stateDir: env.stateDir, workspace: env.workspace });

    assert.equal(result.ok, true);
    assert.equal(result.result.answer, "hello");
    assert.equal((await readEvents(env.stateDir, { runId: result.runId })).some((event) => event.type === "model.completed"), true);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("agent can use a read tool and continue to final answer", async () => {
  const env = await tempEnv();
  try {
    await writeFile(path.join(env.workspace, "note.txt"), "local fact", "utf8");
    const provider = fakeProvider([
      { ok: true, text: "", toolCalls: [{ id: "call_1", name: "fs.read", args: { path: "note.txt" } }] },
      { ok: true, text: "I saw local fact.", toolCalls: [] },
    ]);
    const result = await runAgentTurn({ text: "read note", provider, stateDir: env.stateDir, workspace: env.workspace });
    const messages = await readStateRecords(env.stateDir, "messages.jsonl");
    const snapshots = await readStateRecords(env.stateDir, "context_snapshots.jsonl");
    const modelCalls = await readStateRecords(env.stateDir, "model_calls.jsonl");

    assert.equal(result.ok, true);
    assert.equal(result.result.answer, "I saw local fact.");
    assert.equal(messages.some((message) => message.role === "tool"), true);
    assert.equal(snapshots.length, 2);
    assert.equal(modelCalls.length, 2);
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("agent pauses when a tool requires approval", async () => {
  const env = await tempEnv();
  try {
    const provider = fakeProvider([
      { ok: true, text: "", toolCalls: [{ id: "call_1", name: "fs.write", args: { path: "note.txt", content: "new" } }] },
    ]);
    const result = await runAgentTurn({ text: "write note", provider, stateDir: env.stateDir, workspace: env.workspace });
    const approvals = await readApprovals(env.stateDir, { status: "pending" });

    assert.equal(result.status, "needs_approval");
    assert.equal(approvals.length, 1);
    assert.equal(approvals[0].requestedAction, "fs.write");
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

function fakeProvider(responses) {
  let index = 0;
  return {
    name: "fake",
    model: "fake-model",
    async generate() {
      return responses[index++];
    },
  };
}

async function tempEnv() {
  const root = await mkdtemp(path.join(os.tmpdir(), "agent-core-agent-"));
  return {
    root,
    stateDir: path.join(root, "state"),
    workspace: root,
  };
}
