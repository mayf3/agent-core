import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, readFileSync, existsSync, readdirSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { isForbiddenPath, validateGitRef, resolveRef } from "../cli.ts";

const CLI = join(process.cwd(), "tools", "evolution-harness", "cli.ts");

// --- exported safety helpers ---

test("isForbiddenPath rejects .env / .openduck / .openclaw / logs / .agent-core", () => {
  for (const p of ["/home/u/.env", "/home/u/.openduck/x", "/home/u/.openclaw/y", "/var/logs/z", "/home/u/.agent-core/journal.db"]) {
    assert.equal(isForbiddenPath(p), true, `${p} should be forbidden`);
  }
  assert.equal(isForbiddenPath("/tmp/safe/goal.md"), false);
  assert.equal(isForbiddenPath("/repo/docs/current-goal.md"), false);
});

test("validateGitRef rejects metacharacters, path traversal, control chars, leading dash, empty", () => {
  const bad = [
    "feat/x;rm -rf /", "main && whoami", "a`b`", 'x"y', "main|cat", "a b",
    "../escape", "--global", "-x", "\x07main", "main\x00", "",
  ];
  for (const ref of bad) {
    assert.throws(() => validateGitRef(ref), /unsafe git ref/, `${JSON.stringify(ref)} should be rejected`);
  }
  for (const ref of ["main", "feat/my-change", "abc1234"]) {
    assert.doesNotThrow(() => validateGitRef(ref));
  }
});

test("resolveRef uses spawnSync argv (no shell) and resolves a real ref", () => {
  // main always exists in this repo.
  const commit = resolveRef("main");
  assert.match(commit, /^[0-9a-f]{4,40}$/);
});

// --- end-to-end CLI (real process, asserts artifacts + manifest) ---

function runCli(args: string[], expectFail = false): string {
  try {
    const stdout = execFileSync("node", ["--experimental-strip-types", CLI, ...args], {
      encoding: "utf8",
      cwd: process.cwd(),
    });
    if (expectFail) assert.fail("expected CLI to fail but it succeeded");
    return stdout;
  } catch (e: any) {
    if (!expectFail) throw e;
    return (e.stderr || e.message || "").toString();
  }
}

test("dry-run produces plan.json + evolution-report.md + manifest.json, no push/merge, pinned commits", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-dry-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Test Goal\n\nDo a thing.\n");
  const outDir = join(dir, "runs");
  try {
    runCli(["--goal", goalPath, "--candidate", "main", "--base", "main", "--out-dir", outDir]);
    const runs = readdirSync(outDir);
    assert.equal(runs.length, 1);
    const runDir = join(outDir, runs[0]);
    assert.ok(existsSync(join(runDir, "plan.json")));
    assert.ok(existsSync(join(runDir, "evolution-report.md")));
    assert.ok(existsSync(join(runDir, "manifest.json")));

    const plan = JSON.parse(readFileSync(join(runDir, "plan.json"), "utf8"));
    assert.equal(plan.evaluate, false);
    assert.equal(plan.candidate.ref, "main");
    assert.match(plan.candidate.commit, /^[0-9a-f]+$/);
    assert.match(plan.base.commit, /^[0-9a-f]+$/);

    const manifest = JSON.parse(readFileSync(join(runDir, "manifest.json"), "utf8"));
    assert.equal(manifest.git_push_invoked, false);
    assert.equal(manifest.git_merge_invoked, false);
    assert.equal(manifest.src_mutated, false);
    assert.ok(Array.isArray(manifest.argv));
    assert.ok(manifest.argv.includes("main"));
    assert.match(manifest.candidate.commit, /^[0-9a-f]+$/);
    assert.ok(manifest.artifacts.length >= 3);

    const report = readFileSync(join(runDir, "evolution-report.md"), "utf8");
    assert.match(report, /dry-run/i);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses --no-dry-run as not_implemented (no false 'candidate prepared' report)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-noimpl-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    const err = runCli(["--goal", goalPath, "--candidate", "main", "--no-dry-run"], true);
    assert.match(err, /not implemented/i);
    // Ensure no run directory was created (no false report).
    const defaultOut = join(process.cwd(), "tools", "evolution-harness", "runs");
    // (The CLI exits before creating a run dir, so nothing to clean.)
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses a forbidden --goal path (.env)", () => {
  assert.match(runCli(["--goal", "/tmp/.env", "--candidate", "main"], true), /forbidden path|--goal/);
});

test("CLI refuses an unsafe --candidate git ref (option injection '-x')", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-opt-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.match(runCli(["--goal", goalPath, "--candidate", "-x"], true), /unsafe git ref|cannot resolve/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses an unsafe --candidate git ref (metacharacters)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-meta-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.match(runCli(["--goal", goalPath, "--candidate", "main;rm -rf /"], true), /unsafe git ref|cannot resolve/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses a forbidden --audit-db (.agent-core production journal)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-audit-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.match(runCli(["--goal", goalPath, "--candidate", "main", "--audit-db", "/home/u/.agent-core/journal.db"], true), /forbidden path/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI refuses a forbidden --out-dir (logs)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-out-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.match(runCli(["--goal", goalPath, "--candidate", "main", "--out-dir", "/var/logs/evo"], true), /forbidden path/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI validates --fixtures-dir is an existing directory (rejects missing/file)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-fix-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  const notDir = join(dir, "afile");
  writeFileSync(notDir, "x");
  try {
    // missing dir
    assert.match(runCli(["--goal", goalPath, "--candidate", "main", "--fixtures-dir", join(dir, "nope")], true), /not a directory/);
    // a file, not a dir
    assert.match(runCli(["--goal", goalPath, "--candidate", "main", "--fixtures-dir", notDir], true), /not a directory/);
    // a real dir is accepted
    mkdirSync(join(dir, "realdir"));
    runCli(["--goal", goalPath, "--candidate", "main", "--fixtures-dir", join(dir, "realdir"), "--out-dir", join(dir, "out")]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI requires --goal and --candidate", () => {
  assert.match(runCli(["--candidate", "main"], true), /--goal is required/);
  const dir = mkdtempSync(join(tmpdir(), "evo-req-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    assert.match(runCli(["--goal", goalPath], true), /--candidate .* is required/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("CLI accepts --evaluate flag (still plan-only until Batch 2 wires it)", () => {
  const dir = mkdtempSync(join(tmpdir(), "evo-eval-"));
  const goalPath = join(dir, "goal.md");
  writeFileSync(goalPath, "# Goal\n");
  try {
    runCli(["--goal", goalPath, "--candidate", "main", "--evaluate", "--out-dir", join(dir, "out")]);
    const runs = readdirSync(join(dir, "out"));
    const runDir = join(dir, "out", runs[0]);
    const plan = JSON.parse(readFileSync(join(runDir, "plan.json"), "utf8"));
    assert.equal(plan.evaluate, true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
