#!/usr/bin/env node
/**
 * Replay/Eval harness CLI (Phase 2 MVP).
 *
 * Drives one candidate build against a fixture, scores it vs a baseline, and
 * writes score.json + report.md. See docs/replay-eval-harness.md.
 *
 * Usage:
 *   node --experimental-strip-types tools/replay-eval/cli.ts \
 *     --fixture path/to/fixture.json \
 *     --candidate feat/my-branch \
 *     [--baseline main] [--out-dir ./out]
 *
 * Safety: ephemeral DB/port/worktree; never production; no secrets; PR-only
 * promotion (this tool only produces the score — it never merges).
 */

import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync, mkdtempSync, rmSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { tmpdir } from "node:os";
import { validateFixture, type Fixture } from "./fixture.ts";
import { scoreFixture, compareFixture, summarize, type FixtureVerdict, type ReplayOutcome } from "./scorer.ts";
import { resolveRef, buildKernel, runFixtureAgainst, DriverError } from "./runner.ts";

/** Reject input/output paths that match forbidden patterns (secrets, config, logs).
 *
 * Uses resolved path segments so that `/tmp/logs`, `./logs/output`, and
 * `x/logs/y` all match, while a file named `logs.txt` at the top level does
 * not. */
function isForbiddenPath(filePath: string): boolean {
  const resolved = resolve(filePath);
  const segments = resolved.replace(/\\/g, "/").split("/");
  return segments.some((seg) =>
    [".env", ".agent-core", ".openduck", ".openclaw", "logs"].includes(seg),
  );
}

/** Reject git refs that contain shell metacharacters or path traversal. */
function validateGitRef(ref: string): void {
  if (/[<>|;&$`'"\\\s]/.test(ref)) {
    throw new Error(`unsafe git ref: ${ref}`);
  }
}

interface Args {
  fixture: string | null;          // null when --fixtures-dir is used
  fixturesDir: string | null;      // null when --fixture is used
  candidate: string;
  baseline: string;
  outDir: string;
}

function parseArgs(argv: string[]): Args {
  const a: Record<string, string> = {};
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i];
    if (k === "--fixture" || k === "--fixtures-dir" || k === "--candidate" || k === "--baseline" || k === "--out-dir") {
      a[k.slice(2).replace("-", "_")] = argv[++i];
    }
  }
  // Exactly one of --fixture / --fixtures-dir is required.
  const hasFixture = !!a.fixture;
  const hasDir = !!a.fixtures_dir;
  if (hasFixture === hasDir) {
    console.error("error: provide exactly one of --fixture <file> or --fixtures-dir <dir>");
    process.exit(2);
  }
  if (hasFixture) {
    if (isForbiddenPath(a.fixture)) {
      console.error(`error: --fixture resolves to a forbidden path (may contain secrets): ${a.fixture}`);
      process.exit(3);
    }
    if (!existsSync(a.fixture) || !statSync(a.fixture).isFile()) {
      console.error(`error: --fixture is not a regular file: ${a.fixture}`);
      process.exit(3);
    }
  } else {
    if (isForbiddenPath(a.fixtures_dir)) {
      console.error(`error: --fixtures-dir resolves to a forbidden path: ${a.fixtures_dir}`);
      process.exit(3);
    }
    if (!existsSync(a.fixtures_dir) || !statSync(a.fixtures_dir).isDirectory()) {
      console.error(`error: --fixtures-dir is not a directory: ${a.fixtures_dir}`);
      process.exit(3);
    }
  }
  if (!a.candidate) {
    console.error("error: --candidate (git ref) is required");
    process.exit(2);
  }
  try {
    validateGitRef(a.candidate);
    if (a.baseline) validateGitRef(a.baseline);
  } catch (e) {
    console.error(`error: ${(e as Error).message}`);
    process.exit(2);
  }
  return {
    fixture: hasFixture ? a.fixture : null,
    fixturesDir: hasDir ? a.fixtures_dir : null,
    candidate: a.candidate,
    baseline: a.baseline ?? "main",
    outDir: a.out_dir ?? process.cwd(),
  };
}

/** Load + validate every *.json fixture in a directory (non-recursive). */
function loadFixturesFromDir(dir: string): Fixture[] {
  const files = readdirSync(dir)
    .filter((f) => f.endsWith(".json"))
    .sort();
  if (files.length === 0) {
    console.error(`error: --fixtures-dir contains no *.json fixtures: ${dir}`);
    process.exit(3);
  }
  const fixtures: Fixture[] = [];
  for (const f of files) {
    const path = join(dir, f);
    try {
      fixtures.push(validateFixture(JSON.parse(readFileSync(path, "utf8"))));
    } catch (e) {
      console.error(`error: invalid fixture ${path}: ${(e as Error).message}`);
      process.exit(3);
    }
  }
  return fixtures;
}

const EXIT_DRIVER_ERROR = 4;

function buildWorktree(ref: string): { dir: string; binary: string; commit: string } {
  const commit = resolveRef(ref);
  const dir = mkdtempSync(join(tmpdir(), "replay-wt-"));
  try {
    execFileSync("git", ["worktree", "add", "--detach", dir, ref], { stdio: "pipe" });
  } catch (e) {
    rmSync(dir, { recursive: true, force: true });
    throw new Error(`cannot create worktree for ${ref}: ${(e as Error).message}`);
  }
  const binary = buildKernel(dir);
  return { dir, binary, commit };
}

function cleanupWorktree(dir: string): void {
  try {
    execFileSync("git", ["worktree", "remove", "--force", dir], { stdio: "pipe" });
  } catch {
    /* best effort */
  }
  try {
    rmSync(dir, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}

async function main() {
  const args = parseArgs(process.argv);

  // Load + validate the fixture list (one file, or a directory of fixtures).
  const fixtures: Fixture[] = args.fixture
    ? [validateFixture(JSON.parse(readFileSync(args.fixture, "utf8")))]
    : loadFixturesFromDir(args.fixturesDir!);

  const ipcToken = randomBytes(16).toString("hex");

  // Validate outDir before starting expensive worktrees.
  if (isForbiddenPath(args.outDir)) {
    console.error(`error: --out-dir resolves to a forbidden path: ${args.outDir}`);
    process.exit(4);
  }
  const outDirResolved = resolve(args.outDir);
  try {
    mkdirSync(outDirResolved, { recursive: true });
    // Verify we can write by trying to create a temp file.
    const probe = join(outDirResolved, `.probe-${randomBytes(4).toString("hex")}`);
    writeFileSync(probe, "");
    rmSync(probe);
  } catch {
    console.error(`error: --out-dir is not writable: ${args.outDir}`);
    process.exit(4);
  }

  // Build candidate + baseline worktrees.
  let candidateWt: { dir: string; binary: string; commit: string } | null = null;
  let baselineWt: { dir: string; binary: string; commit: string } | null = null;
  try {
    console.error(`building candidate (${args.candidate})...`);
    candidateWt = buildWorktree(args.candidate);
    console.error(`building baseline (${args.baseline})...`);
    baselineWt = buildWorktree(args.baseline);
  } catch (e) {
    console.error(`error: ${(e as Error).message}`);
    if (candidateWt) cleanupWorktree(candidateWt.dir);
    process.exit(EXIT_DRIVER_ERROR);
  }

  const fixtureResults: Array<{
    fixture_id: string;
    candidate: ReturnType<typeof scoreFixture>;
    baseline: ReturnType<typeof scoreFixture>;
    delta: number;
    verdict: "improve" | "regress" | "neutral";
  }> = [];
  const errors: Array<{ fixture_id: string; category: string; message: string }> = [];
  let driverFailed = false;
  try {
    for (const fixture of fixtures) {
      let candidateOutcome: ReplayOutcome;
      let baselineOutcome: ReplayOutcome;
      try {
        console.error(`replaying fixture ${fixture.fixture_id} against candidate...`);
        candidateOutcome = await runFixtureAgainst(candidateWt!.binary, fixture, ipcToken);
        console.error(`replaying fixture ${fixture.fixture_id} against baseline...`);
        baselineOutcome = await runFixtureAgainst(baselineWt!.binary, fixture, ipcToken);
      } catch (e) {
        const category = e instanceof DriverError ? e.category : "driver_crash";
        const message = (e as Error).message.slice(0, 200);
        console.error(`error: replay driver failed for ${fixture.fixture_id} (${category}): ${message}`);
        errors.push({ fixture_id: fixture.fixture_id, category, message });
        driverFailed = true;
        continue;
      }
      const candidateScore = scoreFixture(fixture, candidateOutcome);
      const baselineScore = scoreFixture(fixture, baselineOutcome);
      const v = compareFixture(candidateScore, baselineScore);
      fixtureResults.push({
        fixture_id: fixture.fixture_id,
        candidate: candidateScore,
        baseline: baselineScore,
        delta: v.delta,
        verdict: v.verdict,
      });
    }
  } finally {
    if (candidateWt) cleanupWorktree(candidateWt.dir);
    if (baselineWt) cleanupWorktree(baselineWt.dir);
  }
  if (driverFailed) {
    console.error("error: one or more fixture replays failed; partial report written");
  }

  const verdicts: FixtureVerdict[] = fixtureResults.map((r) => ({
    candidate: r.candidate,
    baseline: r.baseline,
    delta: r.delta,
    verdict: r.verdict,
  }));
  const summary = summarize(verdicts);

  const report = {
    meta: {
      generated_at: new Date().toISOString(),
      candidate: args.candidate,
      candidate_commit: candidateWt!.commit,
      baseline: args.baseline,
      baseline_commit: baselineWt!.commit,
      fixture_count_total: fixtures.length,
      fixture_count_scored: fixtureResults.length,
      fixture_errors: errors.length,
    },
    summary,
    fixtures: fixtureResults,
    ...(errors.length > 0 ? { errors } : {}),
  };

  const dir = resolve(args.outDir);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "score.json"), JSON.stringify(report, null, 2) + "\n");
  writeFileSync(join(dir, "report.md"), toMarkdown(report));
  console.error(`replay/eval report written to ${dir}/report.md and score.json`);
  console.log(report.summary.verdict);

  if (driverFailed) {
    console.error("replay/eval completed with driver errors — exiting non-zero");
    process.exitCode = EXIT_DRIVER_ERROR;
  }
}

function toMarkdown(r: any): string {
  const lines: string[] = [];
  lines.push("# Replay/Eval Report");
  lines.push("");
  lines.push(`Candidate: ${r.meta.candidate} (${r.meta.candidate_commit})  Baseline: ${r.meta.baseline} (${r.meta.baseline_commit})`);
  lines.push("");
  if (r.summary.verdict === "no-fixtures") {
    lines.push("## Summary");
    lines.push("");
    lines.push("**SKIPPED** — no fixtures were scored. The driver may have failed for all fixtures.");
    lines.push("");
    if (r.errors && r.errors.length > 0) {
      lines.push("## Errors");
      lines.push("");
      for (const e of r.errors) {
        lines.push(`- **${e.fixture_id}** (${e.category}): ${e.message}`);
      }
      lines.push("");
    }
    return lines.join("\n");
  }

  lines.push("## Summary");
  lines.push("");
  lines.push(`Verdict: **${r.summary.verdict}** (candidate ${r.summary.candidateScore.toFixed(2)} vs baseline ${r.summary.baselineScore.toFixed(2)}, Δ ${r.summary.delta >= 0 ? "+" : ""}${r.summary.delta.toFixed(2)})`);
  lines.push("");
  lines.push("## Per-fixture");
  lines.push("");
  lines.push("| Fixture | Candidate | Baseline | Δ | Verdict |");
  lines.push("|---|---|---|---|---|");
  for (const f of r.fixtures) {
    lines.push(`| ${f.fixture_id} | ${f.candidate.score.toFixed(2)}${f.candidate.hardFail ? " ⚠" : ""} | ${f.baseline.score.toFixed(2)} | ${f.delta.toFixed(2)} | ${f.verdict} |`);
  }
  lines.push("");
  lines.push("_⚠ = candidate hard-fail (regress regardless of score)_");
  lines.push("");
  const hardFails = r.fixtures.filter((f: any) => f.candidate.hardFail);
  if (hardFails.length > 0) {
    lines.push("## Candidate hard failures");
    lines.push("");
    for (const f of hardFails) {
      lines.push(`### ${f.fixture_id}`);
      for (const d of f.candidate.details) {
        lines.push(`- [${d.pass ? "x" : " "}] ${d.name}: ${d.detail}`);
      }
      lines.push("");
    }
  }
  lines.push("");
  if (r.fixtures.length > 0) {
    lines.push("## Candidate expectation details");
    lines.push("");
    for (const d of r.fixtures[0].candidate.details) {
      lines.push(`- [${d.pass ? "x" : " "}] ${d.name}: ${d.detail}`);
    }
    lines.push("");
  }
  if (r.errors && r.errors.length > 0) {
    lines.push("## Errors");
    lines.push("");
    for (const e of r.errors) {
      lines.push(`- **${e.fixture_id}** (${e.category}): ${e.message}`);
    }
    lines.push("");
  }
  return lines.join("\n");
}

main().catch((e) => {
  console.error(`fatal: ${(e as Error).message}`);
  process.exit(1);
});
