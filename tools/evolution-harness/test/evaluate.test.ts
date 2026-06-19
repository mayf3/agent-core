import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { runEvaluation, decide, classifyChildError, sanitizeCategory, classifyHarnessExit, parseSummary, type RunnerResult, type EvalEvidence, type CommandRunner } from "../evaluate.ts";

/** A fake runner that simulates replay-eval/audit-report writing outputs. */
function fakeRunner(overrides: {
  replay?: { status: number; score?: any };
  audit?: { status: number; report?: any };
} = {}): CommandRunner {
  return (argv: string[], _cwd?: string) => {
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
        return { stdout: "", stderr: "", status: overrides.replay.status, errorCode: null };
      }
      return { stdout: "", stderr: "", status: 1, errorCode: null };
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
        return { stdout: "", stderr: "", status: overrides.audit.status, errorCode: null };
      }
      return { stdout: "", stderr: "", status: 1, errorCode: null };
    }
    return { stdout: "", stderr: "", status: 0, errorCode: null };
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
      replay: { status: 0, score: { summary: { verdict: "neutral", candidateScore: 1, baselineScore: 1 }, fixtures: [{ candidate: { hardFail: false } }] } },
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
      replay: { status: 0, score: { summary: { verdict: "regress", candidateScore: 0.5, baselineScore: 0.8 }, fixtures: [{ candidate: { hardFail: false } }] } },
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
      replay: { status: 0, score: { summary: { verdict: "neutral", candidateScore: 1, baselineScore: 1 }, fixtures: [{ candidate: { hardFail: true } }] } },
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

test("evaluate: no-fixtures summary verdict blocks the decision", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-nofixtures-"));
  try {
    const { decision } = runEvaluation(
      { ...inputs(dir, { auditDb: null }), fixturesDir: join(dir, "fixtures") },
      fakeRunner({ replay: { status: 0, score: { summary: { verdict: "no-fixtures" }, fixtures: [] } } }),
    );
    assert.equal(decision, "blocked", "no-fixtures must block");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: runner cwd comes from inputs.repoRoot", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-cwd-"));
  const rootDir = join(dir, "repo");
  mkdirSync(rootDir, { recursive: true });
  mkdirSync(join(dir, "fixtures"));
  let capturedCwd = "";
    const runner: CommandRunner = (argv, cwd) => {
      if (argv.join(" ").includes("replay-eval")) {
        capturedCwd = cwd ?? "";
        // Must write score.json so parseSummary gets valid data.
        const outIdx = argv.indexOf("--out-dir");
        const outDir = outIdx >= 0 ? argv[outIdx + 1] : join(dir, "out");
        mkdirSync(outDir, { recursive: true });
        writeFileSync(join(outDir, "score.json"), JSON.stringify({ summary: { verdict: "neutral", candidateScore: 1, baselineScore: 1 }, fixtures: [] }));
        writeFileSync(join(outDir, "report.md"), "# replay\n");
      }
      return { stdout: "", stderr: "", status: 0, errorCode: null };
    };
    runEvaluation({
      repoRoot: rootDir,
    candidateRef: "main",
    candidateCommit: "abc",
    baseRef: "main",
    baseCommit: "abc",
    fixturesDir: join(dir, "fixtures"),
    auditDb: null,
    runDir: join(dir, "out"),
  }, runner);
  assert.equal(capturedCwd, rootDir, "runner must use inputs.repoRoot as cwd");
  assert.notEqual(capturedCwd, process.cwd(), "must not fall back to process.cwd()");
});

test("evaluate: spawn failure produces error_category on child (internal_error at CLI layer)", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-spawnfail-"));
  try {
    const runner: CommandRunner = (_argv, _cwd) => {
      return { stdout: "", stderr: "", status: null, errorCode: "spawn_failure" };
    };
    const { evidence } = runEvaluation(
      { ...inputs(dir, { auditDb: null }), fixturesDir: join(dir, "fixtures") },
      runner,
    );
    assert.equal(evidence.children.length, 1);
    assert.equal(evidence.children[0].error_category, "spawn_failure");
    assert.equal(evidence.children[0].exit_code, null);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: non-zero child exit sets driver_failure error_category", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-driverfail-"));
  try {
    const runner: CommandRunner = (_argv, _cwd) => {
      return { stdout: "", stderr: "", status: 4, errorCode: null };
    };
    const { evidence } = runEvaluation(
      { ...inputs(dir, { auditDb: null }), fixturesDir: join(dir, "fixtures") },
      runner,
    );
    assert.equal(evidence.children.length, 1);
    assert.equal(evidence.children[0].error_category, "driver_failure",
      "non-zero child exit without errorCode must be classified as driver_failure");
    assert.equal(evidence.children[0].exit_code, 4);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("evaluate: non-zero child with score.json evidence still links artifacts", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-driver-evidence-"));
  try {
    const runner: CommandRunner = (argv, _cwd) => {
      const outIdx = argv.indexOf("--out-dir");
      const outDir = outIdx >= 0 ? argv[outIdx + 1] : "";
      if (outDir) {
        mkdirSync(outDir, { recursive: true });
        // Write a score.json on non-zero exit (simulates real replay-eval behavior).
        writeFileSync(join(outDir, "score.json"), JSON.stringify({
          summary: { verdict: "no-fixtures" },
          fixtures: [],
          errors: [{ fixture_id: "f1", category: "ingress_failed", message: "safe" }],
        }));
        writeFileSync(join(outDir, "report.md"), "# replay report\n");
      }
      return { stdout: "", stderr: "", status: 4, errorCode: null };
    };
    const { decision, evidence } = runEvaluation(
      { ...inputs(dir, { auditDb: null }), fixturesDir: join(dir, "fixtures") },
      runner,
    );
    // Evidence was parsed from score.json even though child exited non-zero.
    assert.equal(evidence.replay.summary?.verdict, "no-fixtures");
    assert.equal(evidence.replay.exitCode, 4);
    assert.ok(evidence.artifacts.some((a) => a.kind === "replay-score"), "score.json must be linked");
    assert.ok(evidence.artifacts.some((a) => a.kind === "replay-report"), "report.md must be linked");
    // Error categories from score.json must be surfaced.
    assert.ok(evidence.replay.errorCategories?.includes("ingress_failed"), "error categories from score.json must be surfaced");
    // Decide() still blocks because verdict is no-fixtures.
    assert.equal(decision, "blocked");
    // Child has driver_failure error_category for CLI exit code classification.
    assert.equal(evidence.children[0].error_category, "driver_failure");
    assert.equal(evidence.children[0].artifacts_produced, 2);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- classifyChildError exact exit-code matrix ---

test("classifyChildError: zero-exit ok returns null", () => {
  assert.equal(classifyChildError({ status: 0, errorCode: null } as RunnerResult), null);
});

test("classifyChildError: spawn_failure returns spawn_failure", () => {
  assert.equal(classifyChildError({ status: null, errorCode: "spawn_failure" } as RunnerResult), "spawn_failure");
});

test("classifyChildError: timeout errorCode returns timeout", () => {
  assert.equal(classifyChildError({ status: null, errorCode: "timeout" } as RunnerResult), "timeout");
});

test("classifyChildError: status null with no errorCode returns timeout", () => {
  assert.equal(classifyChildError({ status: null, errorCode: null } as RunnerResult), "timeout");
});

test("classifyChildError: non-zero status with no errorCode returns driver_failure", () => {
  assert.equal(classifyChildError({ status: 4, errorCode: null } as RunnerResult), "driver_failure");
  assert.equal(classifyChildError({ status: 1, errorCode: null } as RunnerResult), "driver_failure");
});

test("classifyChildError: non-zero status with errorCode preserves errorCode", () => {
  assert.equal(classifyChildError({ status: 1, errorCode: "spawn_failure" } as RunnerResult), "spawn_failure");
});

test("classifyChildError: arbitrary errorCode is sanitized through whitelist", () => {
  assert.equal(classifyChildError({ status: 1, errorCode: "arbitrary-os-detail" } as RunnerResult), "internal_driver_error");
  assert.equal(classifyChildError({ status: null, errorCode: "unknown-error" } as RunnerResult), "internal_driver_error");
});

// --- sanitizeCategory ---

test("sanitizeCategory: known categories pass through", () => {
  assert.equal(sanitizeCategory("ingress_failed"), "ingress_failed");
  assert.equal(sanitizeCategory("port_binding"), "port_binding");
  assert.equal(sanitizeCategory("driver_failure"), "driver_failure");
});

test("sanitizeCategory: unknown category maps to internal_driver_error", () => {
  assert.equal(sanitizeCategory("arbitrary stderr text"), "internal_driver_error");
  assert.equal(sanitizeCategory(""), "internal_driver_error");
});

// --- parseSummary ---

test("parseSummary: valid regress with scores succeeds", () => {
  const s = parseSummary({ verdict: "regress", candidateScore: 0.5, baselineScore: 0.8 });
  assert.ok(s, "valid regress must parse");
  assert.equal(s!.verdict, "regress");
  assert.equal(s!.candidateScore, 0.5);
  assert.equal(s!.baselineScore, 0.8);
  assert.ok(typeof s!.delta === "number");
  assert.ok(Math.abs(s!.delta - (-0.3)) < 0.001, `delta ${s!.delta} !== -0.3`);
});

test("parseSummary: valid no-fixtures succeeds without numeric fields", () => {
  const s = parseSummary({ verdict: "no-fixtures" });
  assert.ok(s, "valid no-fixtures must parse");
  assert.equal(s!.verdict, "no-fixtures");
  assert.equal(s!.candidateScore, undefined);
});

test("parseSummary: unknown verdict returns undefined", () => {
  assert.equal(parseSummary({ verdict: "bogus" }), undefined);
  assert.equal(parseSummary({ verdict: "impossible" }), undefined);
  assert.equal(parseSummary({ verdict: "pass" }), undefined);
});

test("parseSummary: string score returns undefined", () => {
  assert.equal(parseSummary({ verdict: "regress", candidateScore: "0.5", baselineScore: 0.8 }), undefined);
});

test("parseSummary: missing scores on regress returns undefined", () => {
  assert.equal(parseSummary({ verdict: "regress" }), undefined);
});

test("parseSummary: null/undefined/invalid input returns undefined", () => {
  assert.equal(parseSummary(null), undefined);
  assert.equal(parseSummary(undefined), undefined);
  assert.equal(parseSummary("string"), undefined);
});

// --- classifyHarnessExit ---

test("classifyHarnessExit: clean evidence -> exit 0", () => {
  const evidence = { children: [] } as unknown as EvalEvidence;
  assert.equal(classifyHarnessExit(evidence, "pass").code, 0);
  assert.equal(classifyHarnessExit(null, null).code, 0);
});

test("classifyHarnessExit: any child error -> exit 5", () => {
  const evidence = {
    children: [{ error_category: "driver_failure" }],
  } as unknown as EvalEvidence;
  assert.equal(classifyHarnessExit(evidence, "blocked").code, 5, "error wins over blocked");
  assert.equal(classifyHarnessExit(evidence, "pass").code, 5);
});

test("classifyHarnessExit: genuine red-line with clean children -> exit 10", () => {
  const evidence = {
    children: [{ error_category: null }],
  } as unknown as EvalEvidence;
  assert.equal(classifyHarnessExit(evidence, "blocked").code, 10);
});

// --- audit non-zero exit classification ---

test("evaluate: non-zero audit exit classified as driver_failure (consistent with replay)", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-audit-driver-"));
  try {
    const runner: CommandRunner = (argv, _cwd) => {
      if (argv.join(" ").includes("audit-report")) {
        return { stdout: "", stderr: "", status: 1, errorCode: null };
      }
      return { stdout: "", stderr: "", status: 0, errorCode: null };
    };
    const { evidence } = runEvaluation(
      { ...inputs(dir), fixturesDir: null, auditDb: join(dir, "snap.db") },
      runner,
    );
    assert.equal(evidence.children.length, 1);
    assert.equal(evidence.children[0].command, "audit-report");
    assert.equal(evidence.children[0].error_category, "driver_failure",
      "audit non-zero must be classified as driver_failure like replay");
    assert.equal(evidence.children[0].exit_code, 1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- replay environment isolation ---

test("evaluate: runner cwd comes from inputs.repoRoot", () => {
  const dir = mkdtempSync(join(tmpdir(), "eval-pin-"));
  let seenCandidate = "";
  let seenBase = "";
  try {
    mkdirSync(join(dir, "fixtures"));
    const runner: CommandRunner = (argv, _cwd) => {
      const cIdx = argv.indexOf("--candidate");
      const bIdx = argv.indexOf("--baseline");
      if (cIdx >= 0) seenCandidate = argv[cIdx + 1];
      if (bIdx >= 0) seenBase = argv[bIdx + 1];
      return fakeRunner({ replay: { status: 0, score: { summary: { verdict: "neutral", candidateScore: 1, baselineScore: 1 }, fixtures: [] } } })(argv);
    };
    runEvaluation({ ...inputs(dir), candidateCommit: "deadbee", baseCommit: "feedface" }, runner);
    assert.equal(seenCandidate, "deadbee", "candidate pinned to commit");
    assert.equal(seenBase, "feedface", "base pinned to commit");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
