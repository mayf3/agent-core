import test from "node:test";
import assert from "node:assert/strict";
import { freePort, DriverError, startKernel, waitForReady } from "../runner.ts";
import { mkdtempSync, rmSync, existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

// --- port binding ---

test("freePort returns a valid port number", async () => {
  const port = await freePort();
  assert.ok(port > 0 && port <= 65535, `freePort returned ${port}`);
});

test("freePort multiple calls return distinct ports", async () => {
  const ports = await Promise.all([freePort(), freePort(), freePort()]);
  const unique = new Set(ports);
  assert.equal(unique.size, ports.length, "each port should be unique");
  for (const p of unique) {
    assert.ok(p > 0 && p <= 65535);
  }
});

// --- DriverError ---

test("DriverError has the correct name and category", () => {
  const e = new DriverError("port_binding", "could not bind");
  assert.equal(e.name, "DriverError");
  assert.equal(e.category, "port_binding");
  assert.match(e.message, /could not bind/);
});

// --- startKernel error handling ---

test("startKernel on nonexistent binary creates temp dir but kernel never becomes ready", async () => {
  const dir = mkdtempSync(join(tmpdir(), "runner-test-"));
  const nonexistent = join(dir, "does-not-exist");
  try {
    const handle = await startKernel(nonexistent, "test-token");
    // spawn doesn't throw synchronously; the child emits 'error' which is
    // handled by the on('error') handler. waitForReady will time out.
    await assert.rejects(
      () => waitForReady(handle, 1000),
      (err: unknown) => {
        if (!(err instanceof DriverError)) return false;
        return err.category === "kernel_not_ready";
      },
    );
  } finally {
    try { rmSync(dir, { recursive: true, force: true }); } catch { /* best effort */ }
  }
});

// --- cli-level DriverError propagation ---

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
