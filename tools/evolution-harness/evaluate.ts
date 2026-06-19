/**
 * Evaluation composition (Batch 2). Composes tools/replay-eval + optional
 * tools/audit-report into an evidence package and derives a pass/blocked
 * decision from the red-lines.
 *
 * Safety: all child processes are spawned WITHOUT a shell (spawnSync + argv).
 * The runner is injectable so tests never start a real service / network /
 * production DB.
 */

import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";

export interface RunnerResult {
  stdout: string;
  stderr: string;
  status: number | null;
  /** Error code from spawnSync (e.g. "ENOENT", "ETIMEDOUT") or null on success. */
  errorCode: string | null;
}

/** A child-command runner. Injectable for tests. */
export type CommandRunner = (argv: string[], cwd?: string) => RunnerResult;

/** Default runner: spawnSync + argv, NO shell, with repo cwd/timeout/maxBuffer. */
export const defaultRunner: CommandRunner = (argv, cwd) => {
  const [cmd, ...rest] = argv;
  const r = spawnSync(cmd, rest, {
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
    cwd: cwd ?? process.cwd(),
    timeout: 600_000,
    maxBuffer: 10 * 1024 * 1024,
  });
  let errorCode: string | null = null;
  if (r.error) {
    errorCode = r.error.code === "ENOENT" ? "spawn_failure" : (r.error.code ?? "spawn_failure");
  }
  if (r.signal === "SIGTERM") {
    errorCode = "timeout";
  }
  return { stdout: r.stdout ?? "", stderr: r.stderr ?? "", status: r.status, errorCode };
};

export interface EvalInputs {
  /** Absolute path to the repo root (cwd for replay-eval/audit-report). */
  repoRoot: string;
  candidateRef: string;
  candidateCommit: string;
  baseRef: string;
  baseCommit: string;
  /** fixtures dir for replay-eval, or null to skip replay. */
  fixturesDir: string | null;
  /** copied SQLite snapshot for audit-report, or null to skip audit. */
  auditDb: string | null;
  /** run directory to copy evidence into. */
  runDir: string;
}

export interface ChildRunInfo {
  command: string;
  argv: string[];
  exit_code: number | null;
  started_at: string;
  finished_at: string;
  /** Structured category e.g. "timeout", "spawn_failure", or null on success. */
  error_category: string | null;
  artifacts_produced: number;
}

export interface EvalEvidence {
  replay: {
    ran: boolean;
    exitCode: number | null;
    summary?: { verdict?: "improve" | "regress" | "neutral" | "no-fixtures" };
    anyHardFail?: boolean;
    /** Whitelisted driver error categories from score.json.errors[].category. */
    errorCategories?: string[];
  };
  audit: {
    ran: boolean;
    exitCode: number | null;
    redLines?: {
      hashChainFaulty: boolean;
      unknownDispatches: boolean;
      projectionDrift: boolean;
      undeliveredIngress: boolean;
    };
  };
  children: ChildRunInfo[];
  artifacts: { path: string; kind: string }[];
}

export type Decision = "pass" | "blocked";

/**
 * Run replay-eval (if fixturesDir) and audit-report (if auditDb) via the
 * runner, copy their outputs into runDir, and derive a decision. NEVER
 * commits/merges/pushes — this function only reads + writes files in runDir.
 */
export function runEvaluation(inputs: EvalInputs, runner: CommandRunner = defaultRunner): { evidence: EvalEvidence; decision: Decision } {
  const children: ChildRunInfo[] = [];
  const evidence: EvalEvidence = {
    replay: { ran: false, exitCode: null },
    audit: { ran: false, exitCode: null },
    children,
    artifacts: [],
  };

  // 1. replay-eval (if fixtures provided).
  if (inputs.fixturesDir) {
    const replayOut = join(inputs.runDir, "replay");
    const argv = [
      "node", "--experimental-strip-types",
      join(inputs.repoRoot, "tools", "replay-eval", "cli.ts"),
      "--fixtures-dir", inputs.fixturesDir,
      "--candidate", inputs.candidateCommit,
      "--baseline", inputs.baseCommit,
      "--out-dir", replayOut,
    ];
    const startedAt = new Date().toISOString();
    const r = runner(argv, inputs.repoRoot);
    const finishedAt = new Date().toISOString();
    // Non-zero child exit without a spawn error is a driver/infrastructure failure.
    let errorCategory: string | null = r.errorCode;
    if (r.status !== 0 && errorCategory === null) errorCategory = "driver_failure";
    evidence.replay = { ran: true, exitCode: r.status };
    const scorePath = join(replayOut, "score.json");
    const reportPath = join(replayOut, "report.md");
    let artifactsProduced = 0;
    // Discover artifacts by existence regardless of exit code.
    const replayErrors: string[] = [];
    if (existsSync(scorePath)) {
      try {
        const score = JSON.parse(readFileSync(scorePath, "utf8"));
        evidence.replay.summary = score.summary;
        evidence.replay.anyHardFail = Array.isArray(score.fixtures) && score.fixtures.some((f: any) => f.candidate?.hardFail);
        // Surface per-fixture driver error categories from score.json.
        if (Array.isArray(score.errors)) {
          for (const e of score.errors) {
            if (typeof e.category === "string" && e.category.length > 0) {
              replayErrors.push(e.category);
            }
          }
        }
      } catch {
        // malformed score.json — leave summary undefined; decision stays conservative.
      }
      evidence.artifacts.push({ path: "replay/score.json", kind: "replay-score" });
      artifactsProduced++;
    }
    if (existsSync(reportPath)) {
      evidence.artifacts.push({ path: "replay/report.md", kind: "replay-report" });
      artifactsProduced++;
    }
    evidence.replay.errorCategories = replayErrors.length > 0 ? replayErrors : undefined;
    children.push({
      command: "replay-eval",
      argv,
      exit_code: r.status,
      started_at: startedAt,
      finished_at: finishedAt,
      error_category: errorCategory,
      artifacts_produced: artifactsProduced,
    });
  }

  // 2. audit-report (if a copied snapshot is provided).
  if (inputs.auditDb) {
    const auditOut = join(inputs.runDir, "audit");
    const argv = [
      "node", "--experimental-strip-types",
      join(inputs.repoRoot, "tools", "audit-report", "cli.ts"),
      "--db", inputs.auditDb,
      "--out-dir", auditOut,
    ];
    const startedAt = new Date().toISOString();
    const r = runner(argv, inputs.repoRoot);
    const finishedAt = new Date().toISOString();
    let errorCategory: string | null = r.errorCode;
    // If no errorCode but status is null (e.g. timeout), classify.
    if (r.status === null && errorCategory === null) errorCategory = "timeout";
    evidence.audit = { ran: true, exitCode: r.status };
    const reportJsonPath = join(auditOut, "report.json");
    const reportMdPath = join(auditOut, "report.md");
    let artifactsProduced = 0;
    // Discover artifacts by existence regardless of exit code.
    if (existsSync(reportJsonPath)) {
      try {
        const rep = JSON.parse(readFileSync(reportJsonPath, "utf8"));
        evidence.audit.redLines = {
          hashChainFaulty: rep.hash_chain?.integrity !== "ok",
          unknownDispatches: (rep.unknown_dispatches?.count ?? 0) > 0,
          projectionDrift: (rep.projection_drift?.count ?? 0) > 0,
          undeliveredIngress: (rep.undelivered_ingress?.count ?? 0) > 0,
        };
      } catch {
        // malformed — leave redLines undefined; conservative blocked.
      }
      evidence.artifacts.push({ path: "audit/report.json", kind: "audit-report" });
      artifactsProduced++;
    }
    if (existsSync(reportMdPath)) {
      evidence.artifacts.push({ path: "audit/report.md", kind: "audit-report-md" });
      artifactsProduced++;
    }
    children.push({
      command: "audit-report",
      argv,
      exit_code: r.status,
      started_at: startedAt,
      finished_at: finishedAt,
      error_category: errorCategory,
      artifacts_produced: artifactsProduced,
    });
  }

  const decision = decide(evidence);
  return { evidence, decision };
}

/** Derive pass/blocked from the evidence. Conservative: any evaluated red-line
 *  blocks. Infrastructure/driver errors are classified as internal_error at
 *  the CLI layer; here we only look at what evidence was produced. */
export function decide(evidence: EvalEvidence): Decision {
  // replay red-lines: anyHardFail, regress verdict, no-fixtures, or no summary.
  if (evidence.replay.ran) {
    if (evidence.replay.anyHardFail) return "blocked";
    if (evidence.replay.summary?.verdict === "regress") return "blocked";
    if (evidence.replay.summary?.verdict === "no-fixtures") return "blocked";
    if (!evidence.replay.summary) return "blocked";
  }
  // audit red-lines.
  if (evidence.audit.ran) {
    const rl = evidence.audit.redLines;
    if (!rl) return "blocked";
    if (rl.hashChainFaulty || rl.unknownDispatches || rl.projectionDrift || rl.undeliveredIngress) return "blocked";
  }
  return "pass";
}
