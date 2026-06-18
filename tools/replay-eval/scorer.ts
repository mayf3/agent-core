/**
 * Replay/Eval scorer (Phase 2 replay/eval harness MVP).
 *
 * Scores a replay run's observed outcome against a fixture's soft expectations
 * (see docs/replay-eval-harness.md §6). Each expectation is binary; the score
 * is passes/total. A hard-fail set forces a verdict of "regress" regardless of
 * score: duplicate reply, a forbidden operation, a policy denial when "allow"
 * was expected, or a crash.
 */

import type { Fixture, FixtureExpectations } from "./fixture.ts";

/** What the harness observed from replaying a fixture (read via the audit
 * harness against the ephemeral DB). */
export interface ReplayOutcome {
  /** Distinct operations the Kernel emitted (from journal events). */
  operations: string[];
  /** The reply text the agent produced (concatenated receipt output). */
  replyText: string | null;
  /** Number of outbox dispatches created (for duplicate-reply detection). */
  dispatchCount: number;
  /** Whether the run reached Completed (vs Failed/Unknown/crashed). */
  completed: boolean;
  /** End-to-end latency in ms, if measurable. */
  latencyMs: number | null;
  /** Whether the policy pipeline allowed the intent (vs denied). */
  policyAllowed: boolean | null;
  /** True if the candidate crashed during replay. */
  crashed: boolean;
}

export interface ExpectationResult {
  name: string;
  pass: boolean;
  detail: string;
}

export interface FixtureScore {
  score: number;
  passes: number;
  fails: number;
  details: ExpectationResult[];
  hardFail: boolean;
}

const PASS = (name: string, detail: string): ExpectationResult => ({ name, pass: true, detail });
const FAIL = (name: string, detail: string): ExpectationResult => ({ name, pass: false, detail });

/** Score a single fixture's outcome against its expectations. */
export function scoreFixture(fixture: Fixture, outcome: ReplayOutcome): FixtureScore {
  const exp: FixtureExpectations = fixture.expectations;
  const details: ExpectationResult[] = [];
  let hardFail = false;

  // Crash is a hard fail.
  if (outcome.crashed) {
    hardFail = true;
    details.push(FAIL("no_crash", "candidate crashed during replay"));
  } else {
    details.push(PASS("no_crash", "candidate did not crash"));
  }

  // reply_contains_any
  if (exp.reply_contains_any && exp.reply_contains_any.length > 0) {
    const text = (outcome.replyText ?? "").toLowerCase();
    const hit = exp.reply_contains_any.find((s) => text.includes(s.toLowerCase()));
    details.push(
      hit
        ? PASS("reply_contains_any", `reply contained "${hit}"`)
        : FAIL("reply_contains_any", `reply did not contain any of ${JSON.stringify(exp.reply_contains_any)}`),
    );
  }

  // reply_operations: at least one expected op present.
  if (exp.reply_operations && exp.reply_operations.length > 0) {
    const hit = exp.reply_operations.find((op) => outcome.operations.includes(op));
    details.push(
      hit
        ? PASS("reply_operations", `emitted ${hit}`)
        : FAIL("reply_operations", `did not emit any of ${JSON.stringify(exp.reply_operations)}`),
    );
  }

  // no_duplicate_reply: exactly one dispatch per ingress turn.
  if (exp.no_duplicate_reply) {
    const expected = fixture.turns.length;
    details.push(
      outcome.dispatchCount <= expected
        ? PASS("no_duplicate_reply", `${outcome.dispatchCount} dispatch(es) for ${expected} turn(s)`)
        : (FAIL("no_duplicate_reply", `${outcome.dispatchCount} dispatches for ${expected} turn(s) — duplicate`),
          (hardFail = true)),
    );
  }

  // forbidden_operations: none of these may appear.
  if (exp.forbidden_operations && exp.forbidden_operations.length > 0) {
    const violated = exp.forbidden_operations.filter((op) => outcome.operations.includes(op));
    details.push(
      violated.length === 0
        ? PASS("forbidden_operations", "no forbidden operations emitted")
        : (FAIL("forbidden_operations", `emitted forbidden: ${JSON.stringify(violated)}`),
          (hardFail = true)),
    );
  }

  // policy_verdict
  if (exp.policy_verdict === "allow") {
    const allowed = outcome.policyAllowed === true;
    details.push(
      allowed
        ? PASS("policy_verdict", "intent was allowed")
        : (FAIL("policy_verdict", "intent was denied when allow expected"),
          (hardFail = true)),
    );
  } else if (exp.policy_verdict === "deny") {
    const denied = outcome.policyAllowed === false;
    details.push(
      denied
        ? PASS("policy_verdict", "intent was denied")
        : FAIL("policy_verdict", "intent was allowed when deny expected"),
    );
  }

  // max_latency_ms
  if (typeof exp.max_latency_ms === "number") {
    const lat = outcome.latencyMs;
    details.push(
      lat !== null && lat <= exp.max_latency_ms
        ? PASS("max_latency_ms", `${lat}ms <= ${exp.max_latency_ms}ms`)
        : FAIL("max_latency_ms", `${lat ?? "?"}ms > ${exp.max_latency_ms}ms`),
    );
  }

  const passes = details.filter((d) => d.pass).length;
  const total = details.length;
  return {
    score: total === 0 ? 1 : passes / total,
    passes,
    fails: total - passes,
    details,
    hardFail,
  };
}

export interface FixtureVerdict {
  candidate: FixtureScore;
  baseline: FixtureScore;
  delta: number;
  verdict: "improve" | "regress" | "neutral";
}

/** Compare a candidate score against a baseline score for one fixture. */
export function compareFixture(candidate: FixtureScore, baseline: FixtureScore): FixtureVerdict {
  const delta = candidate.score - baseline.score;
  let verdict: "improve" | "regress" | "neutral";
  if (candidate.hardFail && !baseline.hardFail) verdict = "regress";
  else if (delta > 0) verdict = "improve";
  else if (delta < 0) verdict = "regress";
  else verdict = "neutral";
  return { candidate, baseline, delta, verdict };
}

export interface RunSummary {
  candidateScore: number;
  baselineScore: number;
  delta: number;
  verdict: "improve" | "regress" | "neutral";
}

/** Aggregate per-fixture verdicts into an overall run verdict. */
export function summarize(verdicts: FixtureVerdict[]): RunSummary {
  if (verdicts.length === 0) {
    return { candidateScore: 1, baselineScore: 1, delta: 0, verdict: "neutral" };
  }
  const candidateScore = verdicts.reduce((s, v) => s + v.candidate.score, 0) / verdicts.length;
  const baselineScore = verdicts.reduce((s, v) => s + v.baseline.score, 0) / verdicts.length;
  const delta = candidateScore - baselineScore;
  const anyRegress = verdicts.some((v) => v.verdict === "regress");
  let verdict: "improve" | "regress" | "neutral";
  if (anyRegress && delta < 0) verdict = "regress";
  else if (delta > 0 && !anyRegress) verdict = "improve";
  else verdict = "neutral";
  return { candidateScore, baselineScore, delta, verdict };
}
