import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  appendEvent,
  createEvent,
  createRunRecord,
  getRun,
  readEvents,
  readRuns,
  recordRun,
  resolveStateDir,
  runDoctor,
  updateRunStatus,
} from "../src/index.mjs";

test("records runs and events as JSONL", async () => {
  const stateDir = await tempStateDir();
  try {
    const run = createRunRecord({ runId: "run_test", source: "test", inputSummary: "hello" });
    await recordRun(stateDir, run);
    await appendEvent(stateDir, createEvent("run.started", { runId: run.runId }));

    assert.equal((await readRuns(stateDir)).length, 1);
    assert.equal((await readEvents(stateDir, { runId: run.runId })).length, 1);
    assert.equal((await getRun(stateDir, run.runId)).inputSummary, "hello");
  } finally {
    await rm(stateDir, { recursive: true, force: true });
  }
});

test("updates the latest run status", async () => {
  const stateDir = await tempStateDir();
  try {
    await recordRun(stateDir, createRunRecord({ runId: "run_test", status: "created" }));
    const updated = await updateRunStatus(stateDir, "run_test", "ok", { resultSummary: "done" });

    assert.equal(updated.status, "ok");
    assert.equal((await getRun(stateDir, "run_test")).resultSummary, "done");
  } finally {
    await rm(stateDir, { recursive: true, force: true });
  }
});

test("doctor returns a successful envelope", async () => {
  const stateDir = await tempStateDir();
  try {
    const envelope = await runDoctor({ stateDir });

    assert.equal(envelope.ok, true);
    assert.equal(envelope.result.type, "doctor");
    assert.equal(envelope.result.checks.stateWritable, true);
  } finally {
    await rm(stateDir, { recursive: true, force: true });
  }
});

test("state path defaults under the provided cwd", () => {
  const resolved = resolveStateDir({ cwd: "/tmp/agent-core-cwd" });
  assert.equal(resolved, "/tmp/agent-core-cwd/.agent-core/state");
});

async function tempStateDir() {
  return mkdtemp(path.join(os.tmpdir(), "agent-core-"));
}
