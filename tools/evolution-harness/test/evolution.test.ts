import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, readFileSync, existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { isForbiddenPath, validateGitRef } from "../cli.ts";

const CLI = join(process.cwd(), "tools", "evolution-harness", "cli.ts");

// --- exported safety helpers ---

test("isForbiddenPath rejects .env / .openduck / .openclaw / logs / .agent-core", () => {
  assert.equal(isForbiddenPath("/home/u/.env"), true);
  assert.equal(isForbiddenPath("/home/u/.openduck/x"), true);
  assert.equal(isForbiddenPath("/home/u/.openclaw/y"), true);
  assert.equal(isForbiddenPath("/var/logs/z"), true);
  assert.equal(isForbiddenPath("/home/u/.agent-core/journal.db"), true);
  assert.equal(isForbiddenPath("/tmp/safe/goal.md"), false);
  assert.equal(isForbiddenPath("/repo/docs/current-goal.md"), false);
});

test("validateGitRef rejects shell metacharacters and path traversal", () => {
  for (const bad of ["feat/x;rm -rf /", "main && whoami", "a`b`", 'x"y', "main|cat", "a b", "../escape"]) {
    assert.throws(() => validateGitRef(bad), /unsafe git ref/);
  }
  // Legitimate refs do not throw.
  assert.doesNotThrow(() => validateGitRef("main"));
  assert.doesNotThrow(() => validateGitRef("feat/my-change"));
  assert.doesNotThrow(() => validateGitRef("abc1234"));
});

// --- end-to-end dry-run (uses the repo's real git refs) ---

function runCli(args: string[]): { stdout: string; stderr: string } {
  const stdout = execFileSync("node", ["--experimental-strip-types", CLI, ...args], {
    encoding: "utf8",
    cwd: process.cwd(),
  });
  return { stdout, stderr: "" };
}

test("dry-run produces plan.json + evolution-report.md + manifest.json, no push/merge", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-dry-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Test Goal\n\nDo a thing.\n");
  const outDir = join(dir, "runs");
  try {
    const { stdout } = runCli([
      "--goal", goalPath,
      "--candidate", "main",   // use main (guaranteed resolvable in this repo)
      "--base", "main",
      "--out-dir", outDir,
    ]);
    // Exactly one run dir.
    const runs = readdirSync(outDir);
    assert.equal(runs.length, 1);
    const runDir = join(outDir, runs[0]);
    assert.ok(existsSync(join(runDir, "plan.json")));
    assert.ok(existsSync(join(runDir, "evolution-report.md")));
    assert.ok(existsSync(join(runDir, "manifest.json")));

    const plan = JSON.parse(readFileSync(join(runDir, "plan.json"), "utf8"));
    assert.equal(plan.dry_run, true);
    assert.equal(plan.candidate.ref, "main");
    assert.equal(plan.boundaries.no_auto_merge, true);
    assert.equal(plan.boundaries.no_auto_push, true);
    assert.equal(plan.boundaries.no_src_writes, true);

    const manifest = JSON.parse(readFileSync(join(runDir, "manifest.json"), "utf8"));
    assert.equal(manifest.git_push_invoked, false);
    assert.equal(manifest.git_merge_invoked, false);
    assert.equal(manifest.src_mutated, false);

    const report = readFileSync(join(runDir, "evolution-report.md"), "utf8");
    assert.match(report, /dry-run/i);
    assert.match(report, /manual/i);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses a forbidden --goal path (.env)", () => {
  assert.throws(
    () => runCli(["--goal", "/tmp/.env", "--candidate", "main"]),
    /forbidden path/,
  );
});

test("CLI refuses an unsafe --candidate git ref", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-unsafe-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.throws(
      () => runCli(["--goal", goalPath, "--candidate", "main;rm -rf /"]),
      /unsafe git ref|forbidden path|cannot resolve/,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses a forbidden --audit-db (production journal under .agent-core)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-audit-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.throws(
      () => runCli(["--goal", goalPath, "--candidate", "main", "--audit-db", "/home/u/.agent-core/journal.db"]),
      /forbidden path/,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses a forbidden --out-dir (logs)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-out-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.throws(
      () => runCli(["--goal", goalPath, "--candidate", "main", "--out-dir", "/var/logs/evo"]),
      /forbidden path/,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI requires --goal", () => {
  assert.throws(
    () => runCli(["--candidate", "main"]),
    /--goal is required/,
  );
});

test("CLI requires --candidate", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-nocand-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.throws(
      () => runCli(["--goal", goalPath]),
      /--candidate .* is required/,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
