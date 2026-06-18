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

import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { execSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { tmpdir } from "node:os";
import { validateFixture, type Fixture } from "./fixture.ts";
import { scoreFixture, compareFixture, summarize, type FixtureVerdict, type ReplayOutcome } from "./scorer.ts";
import { resolveRef, buildKernel, runFixtureAgainst } from "./runner.ts";

const FORBIDDEN_PATH_PATTERNS = [
  ".env",
  ".agent-core",
  ".openduck",
  ".openclaw",
  "/logs/",
  "\\logs\\",
];

/** Reject input/output paths that match forbidden patterns (secrets, config, logs). */
function isForbiddenPath(filePath: string): boolean {
  const normalized = filePath.replace(/\\/g, "/");
  return FORBIDDEN_PATH_PATTERNS.some((pat) => normalized.includes(pat));
}

interface Args {
  fixture: string;
  candidate: string;
  baseline: string;
  outDir: string;
}

function parseArgs(argv: string[]): Args {
  const a: Record<string, string> = {};
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i];
    if (k === "--fixture" || k === "--candidate" || k === "--baseline" || k === "--out-dir") {
      a[k.slice(2)] = argv[++i];
    }
  }
  if (!a.fixture) {
    console.error("error: --fixture is required");
    process.exit(2);
  }
  if (isForbiddenPath(a.fixture)) {
    console.error(`error: --fixture resolves to a forbidden path (may contain secrets): ${a.fixture}`);
    process.exit(3);
  }
  if (!existsSync(a.fixture) || !statSync(a.fixture).isFile()) {
    console.error(`error: --fixture is not a regular file: ${a.fixture}`);
    process.exit(3);
  }
  if (!a.candidate) {
    console.error("error: --candidate (git ref) is required");
    process.exit(2);
  }
  // Reject path-traversal characters in the git ref.
  if (/[<>|;&$`'"\\]/.test(a.candidate)) {
    console.error(`error: --candidate contains unsafe characters: ${a.candidate}`);
    process.exit(2);
  }
  return {
    fixture: a.fixture,
    candidate: a.candidate,
    baseline: a.baseline ?? "main",
    outDir: a.outDir ?? process.cwd(),
  };
}

const EXIT_DRIVER_ERROR = 4;

function buildWorktree(ref: string): { dir: string; binary: string; commit: string } {
  const commit = resolveRef(ref);
  const dir = mkdtempSync(join(tmpdir(), "replay-wt-"));
  try {
    execSync(`git worktree add --detach ${dir} ${ref}`, { stdio: "pipe" });
  } catch (e) {
    rmSync(dir, { recursive: true, force: true });
    throw new Error(`cannot create worktree for ${ref}: ${(e as Error).message}`);
  }
  const binary = buildKernel(dir);
  return { dir, binary, commit };
}

function cleanupWorktree(dir: string): void {
  try {
    execSync(`git worktree remove --force ${dir}`, { stdio: "pipe" });
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

  // Load + validate the fixture.
  let fixture: Fixture;
  try {
    fixture = validateFixture(JSON.parse(readFileSync(args.fixture, "utf8")));
  } catch (e) {
    console.error(`error: invalid fixture: ${(e as Error).message}`);
    process.exit(3);
  }

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

  let candidateOutcome: ReplayOutcome;
  let baselineOutcome: ReplayOutcome;
  try {
    console.error(`replaying fixture ${fixture.fixture_id} against candidate...`);
    candidateOutcome = await runFixtureAgainst(candidateWt.binary, fixture, ipcToken);
    console.error(`replaying fixture ${fixture.fixture_id} against baseline...`);
    baselineOutcome = await runFixtureAgainst(baselineWt.binary, fixture, ipcToken);
  } catch (e) {
    console.error(`error: replay driver failed: ${(e as Error).message}`);
    cleanupWorktree(candidateWt.dir);
    cleanupWorktree(baselineWt.dir);
    process.exit(EXIT_DRIVER_ERROR);
  } finally {
    cleanupWorktree(candidateWt.dir);
    cleanupWorktree(baselineWt.dir);
  }

  const candidateScore = scoreFixture(fixture, candidateOutcome);
  const baselineScore = scoreFixture(fixture, baselineOutcome);
  const verdict: FixtureVerdict = compareFixture(candidateScore, baselineScore);
  const summary = summarize([verdict]);

  const report = {
    meta: {
      generated_at: new Date().toISOString(),
      candidate: args.candidate,
      candidate_commit: candidateWt.commit,
      baseline: args.baseline,
      baseline_commit: baselineWt.commit,
      fixture_id: fixture.fixture_id,
      fixture_count: 1,
    },
    summary,
    fixtures: [
      {
        fixture_id: fixture.fixture_id,
        candidate: candidateScore,
        baseline: baselineScore,
        delta: verdict.delta,
        verdict: verdict.verdict,
      },
    ],
  };

  const dir = resolve(args.outDir);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "score.json"), JSON.stringify(report, null, 2) + "\n");
  writeFileSync(join(dir, "report.md"), toMarkdown(report));
  console.error(`replay/eval report written to ${dir}/report.md and score.json`);
  console.log(report.summary.verdict);
}

function toMarkdown(r: any): string {
  const lines: string[] = [];
  lines.push("# Replay/Eval Report");
  lines.push("");
  lines.push(`Candidate: ${r.meta.candidate} (${r.meta.candidate_commit})  Baseline: ${r.meta.baseline} (${r.meta.baseline_commit})`);
  lines.push("");
  lines.push("## Summary");
  lines.push("");
  lines.push(`Verdict: **${r.summary.verdict}** (candidate ${r.summary.candidateScore.toFixed(2)} vs baseline ${r.summary.baselineScore.toFixed(2)}, Δ ${r.summary.delta >= 0 ? "+" : ""}${r.summary.delta.toFixed(2)})`);
  lines.push("");
  lines.push("## Per-fixture");
  lines.push("");
  lines.push("| Fixture | Candidate | Baseline | Δ | Verdict |");
  lines.push("|---|---|---|---|---|");
  for (const f of r.fixtures) {
    lines.push(`| ${f.fixture_id} | ${f.candidate.score.toFixed(2)} | ${f.baseline.score.toFixed(2)} | ${f.delta.toFixed(2)} | ${f.verdict} |`);
  }
  lines.push("");
  lines.push("## Candidate expectation details");
  lines.push("");
  for (const d of r.fixtures[0].candidate.details) {
    lines.push(`- [${d.pass ? "x" : " "}] ${d.name}: ${d.detail}`);
  }
  lines.push("");
  if (r.fixtures[0].candidate.hardFail) {
    lines.push("## Hard failures");
    lines.push("");
    lines.push("- candidate triggered a hard-fail expectation (see details above)");
    lines.push("");
  }
  return lines.join("\n");
}

main().catch((e) => {
  console.error(`fatal: ${(e as Error).message}`);
  process.exit(1);
});
