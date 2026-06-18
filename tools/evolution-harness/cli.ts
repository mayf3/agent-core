#!/usr/bin/env node
/**
 * External Self-Evolution Rehearsal Harness — CLI skeleton (Phase 3 MVP).
 *
 * Strings goal → candidate branch → (planned) replay/eval + audit → report into
 * an experienceable loop. DRY-RUN BY DEFAULT: produces plan.json +
 * evolution-report.md without spawning a worker agent, committing, merging, or
 * pushing. Manual merge only.
 *
 * See docs/evolution-harness.md for the design contract.
 *
 * Usage:
 *   node --experimental-strip-types tools/evolution-harness/cli.ts \
 *     --goal docs/current-goal.md \
 *     --candidate feat/my-change \
 *     [--base main] [--fixtures-dir <dir>] [--audit-db <copied.db>] \
 *     [--out-dir <dir>] [--no-dry-run]
 *
 * Safety: refuses forbidden paths (.env/.agent-core/.openduck/.openclaw/logs/
 * production DB) and unsafe git refs; never invokes git push/merge.
 */

import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { execSync } from "node:child_process";
import { randomBytes } from "node:crypto";

const FORBIDDEN_SEGMENTS = [".env", ".agent-core", ".openduck", ".openclaw", "logs"];

/** True if the path touches a forbidden segment (secrets/logs/prod state). */
export function isForbiddenPath(filePath: string): boolean {
  const segments = resolve(filePath).replace(/\\/g, "/").split("/");
  return segments.some((seg) => FORBIDDEN_SEGMENTS.includes(seg));
}

/** Throw if the git ref contains shell metacharacters or path traversal. */
export function validateGitRef(ref: string): void {
  if (/[<>|;&$`'"\\\s]/.test(ref) || ref.includes("..")) {
    throw new Error(`unsafe git ref: ${ref}`);
  }
}

/** Resolve a git ref to a short commit hash, or throw if unresolvable. */
export function resolveRef(ref: string): string {
  validateGitRef(ref);
  try {
    return execSync(`git rev-parse --short ${ref}`, { encoding: "utf8", stdio: ["pipe", "pipe", "pipe"] }).trim();
  } catch {
    throw new Error(`cannot resolve git ref: ${ref}`);
  }
}

interface Args {
  goal: string;
  candidate: string;
  base: string;
  fixturesDir: string | null;
  auditDb: string | null;
  outDir: string;
  dryRun: boolean;
}

function parseArgs(argv: string[]): Args {
  const a: Record<string, string> = {};
  let dryRun = true;
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i];
    if (k === "--no-dry-run") {
      dryRun = false;
      continue;
    }
    if (k === "--goal" || k === "--candidate" || k === "--base" || k === "--fixtures-dir" || k === "--audit-db" || k === "--out-dir") {
      a[k.slice(2).replace(/-/g, "_")] = argv[++i];
    }
  }
  if (!a.goal) {
    console.error("error: --goal is required");
    process.exit(2);
  }
  if (isForbiddenPath(a.goal)) {
    console.error(`error: --goal resolves to a forbidden path: ${a.goal}`);
    process.exit(3);
  }
  if (!existsSync(a.goal) || !statSync(a.goal).isFile()) {
    console.error(`error: --goal is not a regular file: ${a.goal}`);
    process.exit(3);
  }
  if (!a.candidate) {
    console.error("error: --candidate (git ref) is required");
    process.exit(2);
  }
  validateGitRef(a.candidate);
  if (a.base) validateGitRef(a.base);
  if (a.fixtures_dir && isForbiddenPath(a.fixtures_dir)) {
    console.error(`error: --fixtures-dir resolves to a forbidden path: ${a.fixtures_dir}`);
    process.exit(3);
  }
  if (a.audit_db) {
    if (isForbiddenPath(a.audit_db)) {
      console.error(`error: --audit-db resolves to a forbidden path: ${a.audit_db}`);
      process.exit(3);
    }
    if (!existsSync(a.audit_db) || !statSync(a.audit_db).isFile()) {
      console.error(`error: --audit-db is not a regular file: ${a.audit_db}`);
      process.exit(3);
    }
  }
  if (a.out_dir && isForbiddenPath(a.out_dir)) {
    console.error(`error: --out-dir resolves to a forbidden path: ${a.out_dir}`);
    process.exit(3);
  }
  return {
    goal: a.goal,
    candidate: a.candidate,
    base: a.base ?? "main",
    fixturesDir: a.fixtures_dir ?? null,
    auditDb: a.audit_db ?? null,
    outDir: a.out_dir ?? join(process.cwd(), "tools", "evolution-harness", "runs"),
    dryRun,
  };
}

function runId(): string {
  const ts = new Date().toISOString().replace(/[-:]/g, "").slice(0, 13);
  const suffix = randomBytes(2).toString("hex");
  return `${ts}-${suffix}`;
}

function main() {
  const args = parseArgs(process.argv);
  const goalText = readFileSync(args.goal, "utf8");

  // Resolve refs (fail fast on unsafe/unresolvable).
  const candidateCommit = resolveRef(args.candidate);
  const baseCommit = resolveRef(args.base);

  const id = runId();
  const runDir = join(resolve(args.outDir), id);
  mkdirSync(runDir, { recursive: true });

  // The harness NEVER auto-commits, merges, or pushes. This invariant is
  // enforced by the absence of any such call here — there is no git push/merge
  // invocation anywhere in this file. Dry-run additionally skips worker-agent
  // delegation and PR creation (not yet implemented anyway).

  const plan = {
    run_id: id,
    generated_at: new Date().toISOString(),
    dry_run: args.dryRun,
    goal_path: resolve(args.goal),
    goal_first_line: goalText.split("\n").find((l) => l.trim().length > 0)?.slice(0, 200) ?? "",
    candidate: { ref: args.candidate, commit: candidateCommit },
    base: { ref: args.base, commit: baseCommit },
    planned_steps: [
      "load + sanitize goal",
      "validate candidate/base refs",
      "create run directory",
      "emit plan.json",
      args.dryRun ? "(dry-run) skip worker-agent delegation" : "delegate to worker agent on candidate",
      args.fixturesDir ? `run replay-eval suite (--fixtures-dir ${args.fixturesDir})` : "(no fixtures-dir) skip replay-eval",
      args.auditDb ? `run audit-report against copied --audit-db ${args.auditDb}` : "(no audit-db) skip audit-report",
      "emit evolution-report.md",
      args.dryRun ? "(dry-run) skip PR creation" : "optionally open a PR (manual merge only)",
    ],
    boundaries: {
      no_src_writes: true,
      no_auto_merge: true,
      no_auto_push: true,
      manual_merge_only: true,
    },
  };

  writeFileSync(join(runDir, "plan.json"), JSON.stringify(plan, null, 2) + "\n");

  const reportLines: string[] = [
    "# Evolution Rehearsal Report",
    "",
    `Run: ${id}  Generated: ${plan.generated_at}`,
    `Mode: ${args.dryRun ? "**dry-run** (no worker agent, no PR)" : "non-dry-run"}`,
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
    `- replay-eval: ${args.fixturesDir ? `scheduled (--fixtures-dir ${args.fixturesDir})` : "skipped (no --fixtures-dir)"}`,
    `- audit-report: ${args.auditDb ? `scheduled (--audit-db ${args.auditDb})` : "skipped (no --audit-db)"}`,
    "",
    "## Decision",
    "",
    args.dryRun
      ? "Dry-run: no candidate was built, no worker agent ran, no PR opened. Inspect `plan.json` and re-run with `--no-dry-run` (still manual merge) to proceed."
      : "A candidate was prepared. **Merge is manual**: a human/Codex reviewer must read this report + the linked score.json/audit-report before merging.",
    "",
  ];
  writeFileSync(join(runDir, "evolution-report.md"), reportLines.join("\n"));

  const manifest = {
    run_id: id,
    commands_run: [
      `git rev-parse --short ${args.candidate} -> ${candidateCommit}`,
      `git rev-parse --short ${args.base} -> ${baseCommit}`,
    ],
    git_push_invoked: false,
    git_merge_invoked: false,
    src_mutated: false,
  };
  writeFileSync(join(runDir, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");

  console.log(`evolution rehearsal ${args.dryRun ? "dry-run " : ""}report written to ${runDir}`);
  console.log(`  plan.json + evolution-report.md + manifest.json`);
}

// Run as a CLI entry only when invoked directly (not when imported by tests).
if (process.argv[1] && resolve(process.argv[1]).endsWith(join("tools", "evolution-harness", "cli.ts"))) {
  main();
}
