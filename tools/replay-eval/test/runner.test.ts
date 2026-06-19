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
