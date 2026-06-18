import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const CLI = resolve(import.meta.dirname, "..", "cli.ts");
const SMOKE_FIXTURE = resolve(import.meta.dirname, "..", "examples", "smoke.json");

function run(args: string[]): { status: number | null; stderr: string } {
  const r = spawnSync("node", ["--experimental-strip-types", CLI, ...args], {
    encoding: "utf8",
    timeout: 10_000,
  });
  return { status: r.status, stderr: r.stderr };
}

// --- git ref safety ---

test("CLI rejects unsafe candidate ref with exit 2 and no build log", () => {
  const { status, stderr } = run([
    "--fixture", SMOKE_FIXTURE,
    "--candidate", "main;echo bad",
    "--baseline", "main",
  ]);
  assert.equal(status, 2, "exit code must be 2");
  assert.match(stderr, /unsafe git ref/);
  assert.doesNotMatch(stderr, /building candidate/);
});

test("CLI rejects unsafe baseline ref with exit 2 and no build log", () => {
  const { status, stderr } = run([
    "--fixture", SMOKE_FIXTURE,
    "--candidate", "main",
    "--baseline", "main;echo bad",
  ]);
  assert.equal(status, 2, "exit code must be 2");
  assert.match(stderr, /unsafe git ref/);
  assert.doesNotMatch(stderr, /building candidate/);
  assert.doesNotMatch(stderr, /building baseline/);
});

// --- forbidden path safety ---

test("CLI rejects forbidden --fixture path (.env) with exit 3", () => {
  const { status, stderr } = run([
    "--fixture", "/tmp/.env",
    "--candidate", "main",
  ]);
  assert.equal(status, 3, "exit code must be 3");
  assert.match(stderr, /forbidden/);
});

test("CLI rejects forbidden --out-dir path (logs) with exit 4", () => {
  const { status, stderr } = run([
    "--fixture", SMOKE_FIXTURE,
    "--candidate", "main",
    "--out-dir", "/tmp/some/logs/path",
  ]);
  assert.equal(status, 4, "exit code must be 4");
  assert.match(stderr, /forbidden/);
});
