import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { runEvaluation, decide, type EvalEvidence, type CommandRunner } from "../evaluate.ts";

/** A fake runner that simulates replay-eval/audit-report writing outputs. */
function fakeRunner(overrides: {
  replay?: { status: number; score?: any };
  audit?: { status: number; report?: any };
} = {}): CommandRunner {
  return (argv: string[]) => {
    const joined = argv.join(" ");
    // replay-eval writes score.json + report.md into its --out-dir.
    if (joined.includes("replay-eval")) {
      const outIdx = argv.indexOf("--out-dir");
      const outDir = outIdx >= 0 ? argv[outIdx + 1] : "";
      if (overrides.replay) {
        if (overrides.replay.status === 0) {
          mkdirSync(outDir, { recursive: true });
          writeFileSync(join(outDir, "score.json"), JSON.stringify(overrides.replay.score ?? { summary: { verdict: "neutral" }, fixtures: [] }));
          writeFileSync(join(outDir, "report.md"), "# replay\n");
        }
        return { stdout: "", stderr: "", status: overrides.replay.status };
      }
      return { stdout: "", stderr: "", status: 1 };
    }
    // audit-report writes report.json + report.md into its --out-dir.
    if (joined.includes("audit-report")) {
      const outIdx = argv.indexOf("--out-dir");
      const outDir = outIdx >= 0 ? argv[outIdx + 1] : "";
      if (overrides.audit) {
        if (overrides.audit.status === 0) {
          mkdirSync(outDir, { recursive: true });
          writeFileSync(join(outDir, "report.json"), JSON.stringify(overrides.audit.report ?? { hash_chain: { integrity: "ok" }, unknown_dispatches: { count: 0 }, projection_drift: { count: 0 }, undelivered_ingress: { count: 0 } }));
          writeFileSync(join(outDir, "report.md"), "# audit\n");
        }
        return { stdout: "", stderr: "", status: overrides.audit.status };
      }
      return { stdout: "", stderr: "", status: 1 };
    }
    return { stdout: "", stderr: "", status: 0 };
  };
}

function inputs(runDir: string, opts: { fixturesDir?: string; auditDb?: string } = {}) {
  return {
    repoRoot: process.cwd(),
    candidateRef: "main",
    candidateCommit: "abc1234",
    baseRef: "main",
    baseCommit: "abc1234",
    fixturesDir: opts.fixturesDir ?? join(runDir, "fixtures"),
    auditDb: opts.auditDb ?? null,
    runDir,
  };
}

test("evaluate: clean replay + clean audit => pass", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-pass-"));
  try {
    mkdirSync(join(dir, "fixtures"));
    // Provide an auditDb so the audit branch actually runs (the fake runner
    // ignores the path content; it just needs runEvaluation to invoke audit).
    const { decision, evidence } = runEvaluation(inputs(dir, { auditDb: join(dir, "snap.db") }), fakeRunner({
      replay: { status: 0, score: { summary: { verdict: "neutral" }, fixtures: [{ candidate: { hardFail: false } }] } },
      audit: { status: 0, report: { hash_chain: { integrity: "ok" }, unknown_dispatches: { count: 0 }, projection_drift: { count: 0 }, undelivered_ingress: { count: 0 } } },
    }));
    assert.equal(decision, "pass");
    assert.equal(evidence.replay.ran, true);
    assert.equal(evidence.audit.ran, true);
    assert.ok(evidence.artifacts.some((a) => a.kind === "replay-score"));
    assert.ok(evidence.artifacts.some((a) => a.kind === "audit-report"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: replay regress => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-regress-"));
  try {
    mkdirSync(join(dir, "fixtures"));
    const { decision } = runEvaluation(inputs(dir), fakeRunner({
      replay: { status: 0, score: { summary: { verdict: "regress" }, fixtures: [{ candidate: { hardFail: false } }] } },
    }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: replay hardFail => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-hardfail-"));
  try {
    mkdirSync(join(dir, "fixtures"));
    const { decision } = runEvaluation(inputs(dir), fakeRunner({
      replay: { status: 0, score: { summary: { verdict: "neutral" }, fixtures: [{ candidate: { hardFail: true } }] } },
    }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: audit hash-chain faulty => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-hash-"));
  try {
    const { decision } = runEvaluation(inputs(dir, { fixturesDir: null }), fakeRunner({
      audit: { status: 0, report: { hash_chain: { integrity: "faulty" }, unknown_dispatches: { count: 0 }, projection_drift: { count: 0 }, undelivered_ingress: { count: 0 } } },
    }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: audit unknown dispatches > 0 => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-unknown-"));
  try {
    const { decision } = runEvaluation(inputs(dir, { fixturesDir: null }), fakeRunner({
      audit: { status: 0, report: { hash_chain: { integrity: "ok" }, unknown_dispatches: { count: 3 }, projection_drift: { count: 0 }, undelivered_ingress: { count: 0 } } },
    }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: audit projection drift > 0 => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-drift-"));
  try {
    const { decision } = runEvaluation(inputs(dir, { fixturesDir: null }), fakeRunner({
      audit: { status: 0, report: { hash_chain: { integrity: "ok" }, unknown_dispatches: { count: 0 }, projection_drift: { count: 2 }, undelivered_ingress: { count: 0 } } },
    }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: audit undelivered ingress > 0 => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-undelivered-"));
  try {
    const { decision } = runEvaluation(inputs(dir, { fixturesDir: null }), fakeRunner({
      audit: { status: 0, report: { hash_chain: { integrity: "ok" }, unknown_dispatches: { count: 0 }, projection_drift: { count: 0 }, undelivered_ingress: { count: 1 } } },
    }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: non-zero replay exit => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-replayfail-"));
  try {
    mkdirSync(join(dir, "fixtures"));
    const { decision } = runEvaluation(inputs(dir), fakeRunner({ replay: { status: 1 } }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: non-zero audit exit => blocked", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-auditfail-"));
  try {
    const { decision } = runEvaluation(inputs(dir, { fixturesDir: null }), fakeRunner({ audit: { status: 1 } }));
    assert.equal(decision, "blocked");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: no fixtures + no audit => no evaluation (decision null at CLI layer)", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-none-"));
  try {
    const { evidence } = runEvaluation({ ...inputs(dir), fixturesDir: null, auditDb: null });
    assert.equal(evidence.replay.ran, false);
    assert.equal(evidence.audit.ran, false);
    // decide() on empty evidence is pass (nothing to block on); the CLI layer
    // reports decision=null (plan-only) because no inputs were provided.
    assert.equal(decide(evidence), "pass");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: pins candidate/base to resolved commits (no ref drift)", () => {
  // The runner receives the COMMIT, not the ref, as --candidate/--baseline.
  const dir = mkdtempSync(join(tmpdir(), "eval-pin-"));
  let seenCandidate = "";
  let seenBase = "";
  try {
    mkdirSync(join(dir, "fixtures"));
    const runner: CommandRunner = (argv) => {
      const cIdx = argv.indexOf("--candidate");
      const bIdx = argv.indexOf("--baseline");
      if (cIdx >= 0) seenCandidate = argv[cIdx + 1];
      if (bIdx >= 0) seenBase = argv[bIdx + 1];
      return fakeRunner({ replay: { status: 0, score: { summary: { verdict: "neutral" }, fixtures: [] } } })(argv);
    };
    runEvaluation({ ...inputs(dir), candidateCommit: "deadbee", baseCommit: "feedface" }, runner);
    assert.equal(seenCandidate, "deadbee", "candidate pinned to commit");
    assert.equal(seenBase, "feedface", "base pinned to commit");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
