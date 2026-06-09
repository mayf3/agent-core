import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { getRun, readApprovals } from "../../core/src/index.mjs";
import { resumeApproval, runTool } from "../src/index.mjs";

test("read tools execute inside the workspace", async () => {
  const env = await tempEnv();
  try {
    await writeFile(path.join(env.workspace, "note.txt"), "hello", "utf8");
    const result = await runTool({
      toolName: "fs.read",
      args: { path: "note.txt" },
      stateDir: env.stateDir,
      workspace: env.workspace,
    });

    assert.equal(result.ok, true);
    assert.equal(result.result.output.content, "hello");
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("write tools pause for approval and resume after approval", async () => {
  const env = await tempEnv();
  try {
    const paused = await runTool({
      toolName: "fs.write",
      args: { path: "out.txt", content: "done" },
      stateDir: env.stateDir,
      workspace: env.workspace,
    });

    assert.equal(paused.status, "needs_approval");
    const approvalId = paused.result.approval.approvalId;
    const resumed = await resumeApproval({ stateDir: env.stateDir, approvalId, decision: "approved" });

    assert.equal(resumed.ok, true);
    assert.equal(await readFile(path.join(env.workspace, "out.txt"), "utf8"), "done");
    assert.equal((await getRun(env.stateDir, paused.runId)).status, "ok");
    const repeated = await resumeApproval({ stateDir: env.stateDir, approvalId, decision: "approved" });
    assert.equal(repeated.ok, false);
    assert.equal(repeated.error.code, "approval_already_decided");
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("shell execution requires approval", async () => {
  const env = await tempEnv();
  try {
    const paused = await runTool({
      toolName: "shell.exec",
      args: { cmd: "printf hi" },
      stateDir: env.stateDir,
      workspace: env.workspace,
    });
    const approvals = await readApprovals(env.stateDir, { status: "pending" });

    assert.equal(paused.status, "needs_approval");
    assert.equal(approvals.length, 1);
    const resumed = await resumeApproval({ stateDir: env.stateDir, approvalId: approvals[0].approvalId, decision: "approved" });
    assert.equal(resumed.result.output.stdout, "hi");
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

test("dangerous shell commands are denied", async () => {
  const env = await tempEnv();
  try {
    const result = await runTool({
      toolName: "shell.exec",
      args: { cmd: "rm -rf /" },
      stateDir: env.stateDir,
      workspace: env.workspace,
    });

    assert.equal(result.ok, false);
    assert.equal(result.error.code, "tool_policy_denied");
  } finally {
    await rm(env.root, { recursive: true, force: true });
  }
});

async function tempEnv() {
  const root = await mkdtemp(path.join(os.tmpdir(), "agent-core-tools-"));
  const workspace = path.join(root, "workspace");
  const stateDir = path.join(root, "state");
  await mkdir(workspace);
  return { root, workspace, stateDir };
}
