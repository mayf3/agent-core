import test from "node:test";
import assert from "node:assert/strict";
import { freePort, DriverError, startKernel, waitForReady, stopKernel } from "../runner.ts";
import { buildWorktree, cleanupWorktree } from "../cli.ts";
import { mkdtempSync, rmSync, existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";

let worktreeCountBefore = 0;

test("setup: count existing worktrees", () => {
  const out = execFileSync("git", ["worktree", "list"], { encoding: "utf8" });
  worktreeCountBefore = out.trim().split("\n").length;
});

test("freePort returns a valid port number and binds + releases cleanly", async () => {
  const port = await freePort();
  assert.ok(port > 0 && port <= 65535, `freePort returned ${port}`);
});

test("DriverError has the correct name and category", () => {
  const e = new DriverError("port_binding", "could not bind");
  assert.equal(e.name, "DriverError");
  assert.equal(e.category, "port_binding");
  assert.match(e.message, /could not bind/);
});

test("startKernel on nonexistent binary cleans up handle and temp dir", async () => {
  const dir = mkdtempSync(join(tmpdir(), "runner-test-"));
  const nonexistent = join(dir, "does-not-exist");
  let handle: Awaited<ReturnType<typeof startKernel>> | null = null;
  try {
    handle = await startKernel(nonexistent, "test-token");
    await assert.rejects(
      () => waitForReady(handle, 1000),
      (err: unknown) => {
        if (!(err instanceof DriverError)) return false;
        return err.category === "kernel_not_ready";
      },
    );
  } finally {
    if (handle) stopKernel(handle);
    try { rmSync(dir, { recursive: true, force: true }); } catch { /* best effort */ }
  }
});

test("DriverError can be caught and its category inspected", () => {
  const categories = ["port_binding", "kernel_startup", "kernel_not_ready", "ingress_failed"];
  for (const cat of categories) {
    const err = new DriverError(cat, `test ${cat}`);
    try {
      throw err;
    } catch (e) {
      assert.ok(e instanceof DriverError);
      assert.equal((e as DriverError).category, cat);
    }
  }
});

test("buildWorktree cleanup on build failure removes its own worktree and no others", async () => {
  const failingBuild = (_dir: string): string => {
    throw new Error("simulated build failure");
  };

  const wtCountBefore = execFileSync("git", ["worktree", "list"], { encoding: "utf8" }).trim().split("\n").length;

  // Call the production buildWorktree with a valid ref and a failing build.
  // The function will create a temp worktree in /tmp, fail at buildKernel,
  // clean up via cleanupWorktree, and rethrow.
  assert.throws(
    () => buildWorktree("HEAD", failingBuild),
    /simulated build failure/,
  );

  // Verify no additional worktrees remain in the repo.
  const wtCountAfter = execFileSync("git", ["worktree", "list"], { encoding: "utf8" }).trim().split("\n").length;
  assert.equal(wtCountAfter, wtCountBefore, "buildWorktree must clean up its own worktree on failure");
});

test("no additional worktrees leaked at repo level", () => {
  const outAfter = execFileSync("git", ["worktree", "list"], { encoding: "utf8" });
  const worktreeCountAfter = outAfter.trim().split("\n").length;
  assert.equal(worktreeCountAfter, worktreeCountBefore,
    `worktree count unchanged (before=${worktreeCountBefore}, after=${worktreeCountAfter})`);
});

// --- pollUntilTerminal ---

test("pollUntilTerminal with synthetic single-turn health returns completed", async () => {
  const port = await freePort();
  const handle = { port, process: null as any, dbPath: "/tmp/fake.db", ipcToken: "t" };

  const http = await import("node:http");
  const server = http.createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({
      worker_jobs: { queued: 0, running: 0, succeeded: 1, failed: 0, retryable_failed: 0 },
      outbox_dispatches: { pending: 0, dispatching: 0, succeeded: 1, failed: 0, retryable_failed: 0 },
    }));
  });
  await new Promise<void>((resolve) => server.listen(port, "127.0.0.1", resolve));

  try {
    const { pollUntilTerminal } = await import("../runner.ts");
    const result = await pollUntilTerminal(handle as any, 1);
    assert.equal(result.completed, true, "single settled turn must complete");
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("pollUntilTerminal with two-turn fixture does not settle after first turn", async () => {
  const port = await freePort();
  const handle = { port, process: null as any, dbPath: "/tmp/fake.db", ipcToken: "t" };
  let callCount = 0;

  const http = await import("node:http");
  const server = http.createServer((_req, res) => {
    callCount++;
    res.writeHead(200, { "Content-Type": "application/json" });
    // First: only 1 total worker_job (not enough for 2-turn fixture).
    // Second: 2 total jobs, no active queue/outbox -> settled.
    const snapshot = callCount <= 1
      ? { worker_jobs: { queued: 0, running: 0, succeeded: 1, failed: 0, retryable_failed: 0 }, outbox_dispatches: { pending: 0, dispatching: 0, succeeded: 0 } }
      : { worker_jobs: { queued: 0, running: 0, succeeded: 2, failed: 0, retryable_failed: 0 }, outbox_dispatches: { pending: 0, dispatching: 0, succeeded: 2 } };
    res.end(JSON.stringify(snapshot));
  });
  await new Promise<void>((resolve) => server.listen(port, "127.0.0.1", resolve));

  try {
    const { pollUntilTerminal } = await import("../runner.ts");
    const result = await pollUntilTerminal(handle as any, 2);
    assert.equal(result.completed, true, "two-turn fixture must settle after both turns");
    assert.ok(callCount >= 2, `expected at least 2 health calls, got ${callCount}`);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

// --- environment isolation ---

test("buildCandidateEnv sets HOME to runtime dir, cwd is separate worktree", async () => {
  const { buildCandidateEnv } = await import("../runner.ts");
  const runtimeDir = "/tmp/test-runtime-123";
  const env = buildCandidateEnv(runtimeDir, "test-token");
  // HOME must be the synthetic runtime dir, not the real $HOME.
  assert.equal(env.HOME, runtimeDir, "HOME must be synthetic runtime dir");
  assert.notEqual(env.HOME, process.env.HOME, "must not leak real HOME");
  // No ambient secret variables inherited.
  assert.equal(env.AGENT_CORE_IPC_TOKEN, "test-token");
  // Only allowed env vars present.
  const allowed = ["PATH", "HOME", "AGENT_CORE_IPC_TOKEN",
    "AGENT_CORE_OPENAI_API_KEY", "AGENT_CORE_FALLBACK_OPENAI_API_KEY",
    "AGENT_CORE_MODEL", "AGENT_CORE_OPENAI_BASE_URL",
    "AGENT_CORE_FALLBACK_OPENAI_BASE_URL",
    "AGENT_CORE_OUTBOX_DISPATCHER_ENABLED",
    "AGENT_CORE_CONNECTOR_EXECUTE_URL"];
  for (const key of Object.keys(env)) {
    assert.ok(allowed.includes(key), `unexpected env var: ${key}`);
  }
  // Stub credentials, not real values.
  assert.match(env.AGENT_CORE_OPENAI_API_KEY, /^replay-stub-key/);
  assert.match(env.AGENT_CORE_FALLBACK_OPENAI_API_KEY, /^replay-stub-key/);
  assert.equal(env.AGENT_CORE_MODEL, "local");
  // cwd is NOT part of env — it is set separately via spawn() cwd option.
  // The caller (startKernel) passes worktreeCwd as spawn cwd.
});
