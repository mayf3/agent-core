#!/usr/bin/env node
/**
 * External Self-Evolution Rehearsal Harness — CLI.
 *
 * Strings goal → candidate ref → (evaluation-only, Batch 2) replay/eval +
 * optional audit → evidence package → pass/blocked decision. **Dry-run by
 * default** (plan + report only). Real evaluation is `--evaluate` (Batch 2);
 * `--no-dry-run` is rejected as not_implemented so it never emits a false
 * "candidate prepared" report.
 *
 * Safety: no shell — all git calls use spawnSync + argv. Refuses forbidden
 * paths and unsafe git refs. Never invokes git push/merge. Manual merge only.
 *
 * See docs/evolution-harness.md for the design contract.
 *
 * Usage:
 *   node --experimental-strip-types tools/evolution-harness/cli.ts \
 *     --goal docs/current-goal.md \
 *     --candidate feat/my-change \
 *     [--base main] [--fixtures-dir <dir>] [--audit-db <copied.db>] \
 *     [--out-dir <dir>] [--evaluate]
 */

import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { runEvaluation, type Decision, type EvalEvidence } from "./evaluate.ts";

const FORBIDDEN_SEGMENTS = [".env", ".agent-core", ".openduck", ".openclaw", "logs"];

/** True if the path touches a forbidden segment (secrets/logs/prod state). */
export function isForbiddenPath(filePath: string): boolean {
  const segments = resolve(filePath).replace(/\\/g, "/").split("/");
  return segments.some((seg) => FORBIDDEN_SEGMENTS.includes(seg));
}

/**
 * Reject a git ref if it is unsafe or structurally invalid. Hardens against:
 * - shell metacharacters (`<>|;&$\`'"\\` and whitespace)
 * - path traversal (`..`)
 * - control characters (0x00–0x1f, 0x7f)
 * - option injection (leading `-`)
 * Unresolvable refs are rejected separately by resolveRef.
 */
export function validateGitRef(ref: string): void {
  if (ref.length === 0) throw new Error("unsafe git ref: empty");
  if (ref.startsWith("-")) throw new Error(`unsafe git ref (option-injection): ${ref}`);
  if (ref.includes("..")) throw new Error(`unsafe git ref (path traversal): ${ref}`);
  if (/[<>|;&$`'"\\\s]/.test(ref)) throw new Error(`unsafe git ref (metacharacters): ${ref}`);
  if (/[\x00-\x1f\x7f]/.test(ref)) throw new Error(`unsafe git ref (control characters): ${ref}`);
}

/** Run `git rev-parse --short <ref>` WITHOUT a shell (spawnSync + argv). Returns
 *  the trimmed short commit, or throws if the ref is unsafe/unresolvable. */
export function resolveRef(ref: string): string {
  validateGitRef(ref);
  const result = spawnSync("git", ["rev-parse", "--short", ref], {
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
  });
  if (result.status !== 0 || !result.stdout) {
    throw new Error(`cannot resolve git ref: ${ref}`);
  }
  return result.stdout.trim();
}

interface Args {
  goal: string;
  candidate: string;
  base: string;
  fixturesDir: string | null;
  auditDb: string | null;
  outDir: string;
  /** Real evaluation mode (Batch 2). Dry-run is the default. */
  evaluate: boolean;
}

const EXIT_BAD_ARG = 2;
const EXIT_FORBIDDEN_OR_MISSING = 3;
const EXIT_NOT_IMPLEMENTED = 4;
/** Evaluation blocked by a red-line (replay regress/hardFail or audit fault). */
const EXIT_BLOCKED = 10;
/** Harness internal error (spawn failure, timeout, etc.), not a candidate problem. */
const EXIT_INTERNAL_ERROR = 5;

function fail(msg: string, code: number): never {
  console.error(`error: ${msg}`);
  process.exit(code);
}

function parseArgs(argv: string[]): Args {
  const a: Record<string, string> = {};
  let evaluate = false;
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i];
    if (k === "--evaluate") {
      evaluate = true;
      continue;
    }
    if (k === "--no-dry-run") {
      // Removed: --no-dry-run never had real execution. Reject explicitly so it
      // cannot produce a false "candidate prepared" report. Use --evaluate.
      fail("--no-dry-run is not implemented; use --evaluate for real evaluation (Batch 2)", EXIT_NOT_IMPLEMENTED);
    }
    if (k === "--goal" || k === "--candidate" || k === "--base" || k === "--fixtures-dir" || k === "--audit-db" || k === "--out-dir") {
      a[k.slice(2).replace(/-/g, "_")] = argv[++i];
    }
  }
  if (!a.goal) fail("--goal is required", EXIT_BAD_ARG);
  if (isForbiddenPath(a.goal)) fail(`--goal resolves to a forbidden path: ${a.goal}`, EXIT_FORBIDDEN_OR_MISSING);
  if (!existsSync(a.goal) || !statSync(a.goal).isFile()) fail(`--goal is not a regular file: ${a.goal}`, EXIT_FORBIDDEN_OR_MISSING);
  if (!a.candidate) fail("--candidate (git ref) is required", EXIT_BAD_ARG);
  try {
    validateGitRef(a.candidate);
  } catch (e) {
    fail((e as Error).message, EXIT_FORBIDDEN_OR_MISSING);
  }
  if (a.base) {
    try {
      validateGitRef(a.base);
    } catch (e) {
      fail((e as Error).message, EXIT_FORBIDDEN_OR_MISSING);
    }
  }
  if (a.fixtures_dir) {
    if (isForbiddenPath(a.fixtures_dir)) fail(`--fixtures-dir resolves to a forbidden path: ${a.fixtures_dir}`, EXIT_FORBIDDEN_OR_MISSING);
    if (!existsSync(a.fixtures_dir) || !statSync(a.fixtures_dir).isDirectory()) fail(`--fixtures-dir is not a directory: ${a.fixtures_dir}`, EXIT_FORBIDDEN_OR_MISSING);
  }
  if (a.audit_db) {
    if (isForbiddenPath(a.audit_db)) fail(`--audit-db resolves to a forbidden path: ${a.audit_db}`, EXIT_FORBIDDEN_OR_MISSING);
    if (!existsSync(a.audit_db) || !statSync(a.audit_db).isFile()) fail(`--audit-db is not a regular file: ${a.audit_db}`, EXIT_FORBIDDEN_OR_MISSING);
  }
  if (a.out_dir && isForbiddenPath(a.out_dir)) fail(`--out-dir resolves to a forbidden path: ${a.out_dir}`, EXIT_FORBIDDEN_OR_MISSING);
  return {
    goal: a.goal,
    candidate: a.candidate,
    base: a.base ?? "main",
    fixturesDir: a.fixtures_dir ?? null,
    auditDb: a.audit_db ?? null,
    outDir: a.out_dir ?? join(process.cwd(), "tools", "evolution-harness", "runs"),
    evaluate,
  };
}

function runId(): string {
  const ts = new Date().toISOString().replace(/[-:]/g, "").slice(0, 13);
  const suffix = randomBytes(2).toString("hex");
  return `${ts}-${suffix}`;
}

export interface RunManifest {
  run_id: string;
  argv: string[];
  started_at: string;
  finished_at: string;
  exit_code: number;
  candidate: { ref: string; commit: string };
  base: { ref: string; commit: string };
  /** "pass" / "blocked" when evaluation ran; null when plan-only. */
  decision: "pass" | "blocked" | null;
  /** Per-child subcommand provenance (replay-eval, audit-report). */
  children: { command: string; argv: string[]; exit_code: number | null; started_at: string; finished_at: string; error_category: string | null; artifacts_produced: number }[];
  artifacts: { path: string; kind: string }[];
  git_push_invoked: boolean;
  git_merge_invoked: boolean;
  src_mutated: boolean;
}

export function main(): RunManifest {
  const startedAt = new Date().toISOString();
  const args = parseArgs(process.argv);
  const goalText = readFileSync(args.goal, "utf8");

  // Resolve refs (fail fast on unsafe/unresolvable) and pin commits.
  let candidateCommit: string;
  let baseCommit: string;
  try {
    candidateCommit = resolveRef(args.candidate);
    baseCommit = resolveRef(args.base);
  } catch (e) {
    fail((e as Error).message, EXIT_FORBIDDEN_OR_MISSING);
  }

  const id = runId();
  const runDir = join(resolve(args.outDir), id);
  mkdirSync(runDir, { recursive: true });

  // The harness NEVER auto-commits, merges, or pushes. This invariant is
  // enforced by the absence of any such call here — there is no git push/merge
  // invocation anywhere in this file.

  const plan = {
    run_id: id,
    generated_at: new Date().toISOString(),
    evaluate: args.evaluate,
    goal_path: resolve(args.goal),
    goal_first_line: goalText.split("\n").find((l) => l.trim().length > 0)?.slice(0, 200) ?? "",
    candidate: { ref: args.candidate, commit: candidateCommit },
    base: { ref: args.base, commit: baseCommit },
    planned_steps: [
      "load + sanitize goal",
      "validate candidate/base refs",
      "create run directory",
      "emit plan.json",
      args.evaluate ? "(evaluate) compose replay-eval/audit-report" : "(dry-run) skip evaluation",
      "emit evolution-report.md",
    ],
    boundaries: {
      no_src_writes: true,
      no_auto_merge: true,
      no_auto_push: true,
      manual_merge_only: true,
    },
  };
  writeFileSync(join(runDir, "plan.json"), JSON.stringify(plan, null, 2) + "\n");

  // Batch 2: real evaluation composition. Only when --evaluate; pins to the
  // resolved commits (no ref drift). NEVER commits/merges/pushes.
  let decision: Decision | null = null;
  let evidence: EvalEvidence | null = null;
  const evalArtifacts: { path: string; kind: string }[] = [];
  if (args.evaluate) {
    if (!args.fixturesDir && !args.auditDb) {
      fail("--evaluate requires at least one of --fixtures-dir or --audit-db", EXIT_BAD_ARG);
    }
    const result = runEvaluation({
      repoRoot: process.cwd(),
      candidateRef: args.candidate,
      candidateCommit,
      baseRef: args.base,
      baseCommit,
      fixturesDir: args.fixturesDir ? resolve(args.fixturesDir) : null,
      auditDb: args.auditDb ? resolve(args.auditDb) : null,
      runDir,
    });
    decision = result.decision;
    evidence = result.evidence;
    evalArtifacts.push(...result.evidence.artifacts);
  }

  const blockedReasons: string[] = [];
  if (evidence?.replay.ran && decision === "blocked") {
    const r = evidence.replay;
    if (r.exitCode !== 0) blockedReasons.push(`replay-eval exited ${r.exitCode} (non-zero)`);
    else if (r.anyHardFail) blockedReasons.push("replay: candidate hardFail detected");
    else if (r.summary?.verdict === "regress") blockedReasons.push(`replay: verdict = regress (candidate ${r.summary.candidateScore.toFixed(2)} vs baseline ${r.summary.baselineScore.toFixed(2)})`);
    else if (!r.summary) blockedReasons.push("replay: no parseable summary (score.json malformed)");
  }
  if (evidence?.audit.ran && decision === "blocked") {
    const a = evidence.audit;
    if (a.exitCode !== 0) blockedReasons.push(`audit-report exited ${a.exitCode} (non-zero)`);
    else if (a.redLines) {
      if (a.redLines.hashChainFaulty) blockedReasons.push("audit: hash-chain faulty");
      if (a.redLines.unknownDispatches) blockedReasons.push("audit: unknown dispatches > 0");
      if (a.redLines.projectionDrift) blockedReasons.push("audit: projection drift > 0");
      if (a.redLines.undeliveredIngress) blockedReasons.push("audit: undelivered ingress > 0");
    } else {
      blockedReasons.push("audit: no parseable report (report.json malformed)");
    }
  }

  const reportChildren = evidence?.children ?? [];

  const reportLines: string[] = [
    "# Evolution Rehearsal Report",
    "",
    `Run: ${id}  Generated: ${plan.generated_at}`,
    `Mode: ${args.evaluate ? "**evaluate** (real evaluation; merge still manual)" : "**dry-run** (no evaluation ran)"}`,
    "",
    "## Plan",
    "",
    `- Goal: \`${plan.goal_path}\``,
    `- Candidate: ${args.candidate} (${candidateCommit})`,
    `- Base: ${args.base} (${baseCommit})`,
    "",
    "Planned steps:",
    ...plan.planned_steps.map((s) => `- ${s}`),
    "",
    "## Red-lines enforced",
    "",
    "- No `.env` / `~/.openduck` / `~/.openclaw` / logs / production DB / secrets reads.",
    "- No service stop/restart.",
    "- No auto-merge, no auto-push to `main`.",
    "- No Kernel `src/` writes unless the goal explicitly targets the Kernel and the change is in a separately-reviewed PR.",
    "- No workflow engine / multi-agent scheduler / shell/browser/deploy.",
    "",
    "## Composition (this run)",
    "",
    `- replay-eval: ${args.fixturesDir ? `requested (--fixtures-dir ${args.fixturesDir})` : "skipped (no --fixtures-dir)"}`,
    `- audit-report: ${args.auditDb ? `requested (--audit-db ${args.auditDb})` : "skipped (no --audit-db)"}`,
    "",
    "## Evidence",
    "",
  ];
  if (reportChildren.length > 0) {
    reportLines.push("| Command | Exit | Error | Artifacts |");
    reportLines.push("|---|---|---|---|");
    for (const c of reportChildren) {
      reportLines.push(`| ${c.command} | ${c.exit_code ?? "?"} | ${c.error_category ?? "-"} | ${c.artifacts_produced} |`);
    }
    reportLines.push("");
  }

  // Linked evidence files.
  if (evalArtifacts.length > 0) {
    reportLines.push("### Artifact links");
    reportLines.push("");
    for (const a of evalArtifacts) {
      reportLines.push(`- [${a.kind}](${a.path})`);
    }
    reportLines.push("");
  }

  reportLines.push("## Decision");
  reportLines.push("");
  reportLines.push(
    decision === null
      ? (args.evaluate
          ? "No evaluation ran (no --fixtures-dir/--audit-db provided)."
          : "Dry-run: no evaluation ran. Re-run with `--evaluate` (+ `--fixtures-dir` and/or `--audit-db`) for real evaluation.")
      : `**${decision.toUpperCase()}**`,
  );
  if (blockedReasons.length > 0) {
    reportLines.push("");
    reportLines.push(`${blockedReasons.length} red-line(s) triggered:`);
    reportLines.push("");
    for (const reason of blockedReasons) {
      reportLines.push(`- ${reason}`);
    }
  }
  reportLines.push("");
  reportLines.push("_Merge is always manual._");
  reportLines.push("");
  writeFileSync(join(runDir, "evolution-report.md"), reportLines.join("\n"));

  // Determine the final exit code before writing the manifest.
  // - blocked by red-line: EXIT_BLOCKED (10)
  // - harness internal error: EXIT_INTERNAL_ERROR (5)
  // - everything else: 0
  let exitCode = 0;
  if (decision === "blocked") {
    exitCode = EXIT_BLOCKED;
  } else if (evidence?.children.some((c) => c.error_category !== null)) {
    exitCode = EXIT_INTERNAL_ERROR;
  }

  const manifest: RunManifest = {
    run_id: id,
    argv: process.argv.slice(2),
    started_at: startedAt,
    finished_at: new Date().toISOString(),
    exit_code: exitCode,
    candidate: { ref: args.candidate, commit: candidateCommit },
    base: { ref: args.base, commit: baseCommit },
    decision,
    children: reportChildren,
    artifacts: [
      { path: "plan.json", kind: "plan" },
      { path: "evolution-report.md", kind: "report" },
      { path: "manifest.json", kind: "manifest" },
      ...evalArtifacts,
    ],
    git_push_invoked: false,
    git_merge_invoked: false,
    src_mutated: false,
  };
  writeFileSync(join(runDir, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");

  console.log(`evolution rehearsal ${args.evaluate ? "evaluate " : "dry-run "}report written to ${runDir}`);
  console.log(`  plan.json + evolution-report.md + manifest.json${decision ? ` (decision: ${decision})` : ""}`);

  // Exit with the computed code so the caller (CI, harness) can distinguish
  // pass (0), blocked (10), and internal error (5).
  if (exitCode !== 0) process.exit(exitCode);
}

// Run as a CLI entry only when invoked directly (not when imported by tests).
if (process.argv[1] && resolve(process.argv[1]).endsWith(join("tools", "evolution-harness", "cli.ts"))) {
  main();
}
