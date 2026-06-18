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

/** A child-command runner. Injectable for tests (returns stdout + exit code). */
export type CommandRunner = (argv: string[]) => { stdout: string; stderr: string; status: number | null };

/** Default runner: spawnSync + argv, NO shell. */
export const defaultRunner: CommandRunner = (argv) => {
  const [cmd, ...rest] = argv;
  const r = spawnSync(cmd, rest, { encoding: "utf8", stdio: ["pipe", "pipe", "pipe"] });
  return { stdout: r.stdout ?? "", stderr: r.stderr ?? "", status: r.status };
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

export interface EvalEvidence {
  replay: {
    ran: boolean;
    exitCode: number | null;
    /** Parsed score.json summary, if replay ran and produced one. */
    summary?: { verdict?: string };
    /** True if any fixture had a hardFail. */
    anyHardFail?: boolean;
  };
  audit: {
    ran: boolean;
    exitCode: number | null;
    /** Red-line booleans derived from report.json, if audit ran. */
    redLines?: {
      hashChainFaulty: boolean;
      unknownDispatches: boolean;
      projectionDrift: boolean;
      undeliveredIngress: boolean;
    };
  };
  artifacts: { path: string; kind: string }[];
}

export type Decision = "pass" | "blocked";

/**
 * Run replay-eval (if fixturesDir) and audit-report (if auditDb) via the
 * runner, copy their outputs into runDir, and derive a decision. NEVER
 * commits/merges/pushes — this function only reads + writes files in runDir.
 */
export function runEvaluation(inputs: EvalInputs, runner: CommandRunner = defaultRunner): { evidence: EvalEvidence; decision: Decision } {
  const evidence: EvalEvidence = {
    replay: { ran: false, exitCode: null },
    audit: { ran: false, exitCode: null },
    artifacts: [],
  };

  // 1. replay-eval (if fixtures provided).
  if (inputs.fixturesDir) {
    const replayOut = join(inputs.runDir, "replay");
    const argv = [
      "node", "--experimental-strip-types",
      join(inputs.repoRoot, "tools", "replay-eval", "cli.ts"),
      "--fixtures-dir", inputs.fixturesDir,
      "--candidate", inputs.candidateCommit, // pin to the resolved commit (no ref drift)
      "--baseline", inputs.baseCommit,
      "--out-dir", replayOut,
    ];
    const r = runner(argv);
    evidence.replay = { ran: true, exitCode: r.status };
    const scorePath = join(replayOut, "score.json");
    if (r.status === 0 && existsSync(scorePath)) {
      try {
        const score = JSON.parse(readFileSync(scorePath, "utf8"));
        evidence.replay.summary = score.summary;
        evidence.replay.anyHardFail = Array.isArray(score.fixtures) && score.fixtures.some((f: any) => f.candidate?.hardFail);
      } catch {
        // malformed score.json — leave summary undefined; decision stays conservative (blocked).
      }
      evidence.artifacts.push({ path: "replay/score.json", kind: "replay-score" });
      evidence.artifacts.push({ path: "replay/report.md", kind: "replay-report" });
    }
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
    const r = runner(argv);
    evidence.audit = { ran: true, exitCode: r.status };
    const reportPath = join(auditOut, "report.json");
    if (r.status === 0 && existsSync(reportPath)) {
      try {
        const rep = JSON.parse(readFileSync(reportPath, "utf8"));
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
      evidence.artifacts.push({ path: "audit/report.md", kind: "audit-report-md" });
    }
  }

  const decision = decide(evidence);
  return { evidence, decision };
}

/** Derive pass/blocked from the evidence. Conservative: any red-line or any
 *  non-zero exit or missing summary blocks. */
export function decide(evidence: EvalEvidence): Decision {
  // replay red-lines: regress verdict, any hardFail, or non-zero exit.
  if (evidence.replay.ran) {
    if (evidence.replay.exitCode !== 0) return "blocked";
    if (evidence.replay.anyHardFail) return "blocked";
    if (evidence.replay.summary?.verdict === "regress") return "blocked";
    // If replay ran but produced no parseable summary, block conservatively.
    if (!evidence.replay.summary) return "blocked";
  }
  // audit red-lines.
  if (evidence.audit.ran) {
    if (evidence.audit.exitCode !== 0) return "blocked";
    const rl = evidence.audit.redLines;
    if (!rl) return "blocked";
    if (rl.hashChainFaulty || rl.unknownDispatches || rl.projectionDrift || rl.undeliveredIngress) return "blocked";
  }
  return "pass";
}
