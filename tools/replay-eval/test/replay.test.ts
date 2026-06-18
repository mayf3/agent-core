import test from "node:test";
import assert from "node:assert/strict";
import { validateFixture, type Fixture } from "../fixture.ts";
import {
  scoreFixture,
  compareFixture,
  summarize,
  type ReplayOutcome,
  type FixtureScore,
} from "../scorer.ts";

function validFixture(overrides: Partial<Fixture> = {}): Fixture {
  return {
    schema_version: 1,
    fixture_id: "test-fixture-1",
    description: "test",
    source: { kind: "authored" },
    setup: { agent_id: "main", channel: "feishu", session_id: "s1" },
    turns: [{ role: "user", external_event_id: "m1", text: "hi" }],
    expectations: {
      reply_contains_any: ["hello"],
      reply_operations: ["feishu.send_message"],
      no_duplicate_reply: true,
      forbidden_operations: ["shell.exec"],
      policy_verdict: "allow",
    },
    ...overrides,
  } as Fixture;
}

// --- fixture validation ---

test("validateFixture accepts a well-formed fixture", () => {
  assert.doesNotThrow(() => validateFixture(validFixture()));
});

test("validateFixture rejects a bad schema_version", () => {
  assert.throws(
    () => validateFixture({ ...validFixture(), schema_version: 99 }),
    /schema_version must be 1/,
  );
});

test("validateFixture rejects a fixture with no turns", () => {
  assert.throws(
    () => validateFixture({ ...validFixture(), turns: [] }),
    /turns must be a non-empty array/,
  );
});

test("validateFixture accepts a smoke fixture (empty expectations)", () => {
  const f = validFixture({ expectations: {} });
  assert.doesNotThrow(() => validateFixture(f));
});

// --- fixture validation — edge cases ---

test("validateFixture rejects a null fixture", () => {
  assert.throws(() => validateFixture(null), /fixture must be a JSON object/);
});

test("validateFixture rejects a non-object (string)", () => {
  assert.throws(() => validateFixture("not-a-fixture"), /fixture must be a JSON object/);
});

test("validateFixture rejects bad source.kind", () => {
  assert.throws(
    () => validateFixture({ ...validFixture(), source: { kind: "invalid" } }),
    /source.kind must be "audited" or "authored"/,
  );
});

test("validateFixture rejects missing fixture_id", () => {
  assert.throws(
    () => validateFixture({ ...validFixture(), fixture_id: "" }),
    /fixture_id must be a non-empty string/,
  );
});

function goodOutcome(): ReplayOutcome {
  return {
    operations: ["feishu.send_message"],
    replyText: "hello there",
    dispatchCount: 1,
    completed: true,
    latencyMs: 100,
    policyAllowed: true,
    crashed: false,
  };
}

test("scoreFixture: all expectations pass = score 1.0, no hard fail", () => {
  const s = scoreFixture(validFixture(), goodOutcome());
  assert.equal(s.hardFail, false);
  assert.equal(s.fails, 0);
  assert.equal(s.score, 1.0);
});

test("scoreFixture: reply_contains_any miss lowers score but is not hard fail", () => {
  const o = { ...goodOutcome(), replyText: "totally unrelated text" };
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, false);
  assert.ok(s.score < 1.0, "score should drop");
  assert.ok(s.fails > 0);
});

test("scoreFixture: forbidden operation emitted is a hard fail", () => {
  const o = { ...goodOutcome(), operations: ["feishu.send_message", "shell.exec"] };
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
});

test("scoreFixture: duplicate reply is a hard fail", () => {
  const o = { ...goodOutcome(), dispatchCount: 3 }; // 3 dispatches for 1 turn
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
});

test("scoreFixture: crash is a hard fail", () => {
  const o = { ...goodOutcome(), crashed: true };
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
});

test("scoreFixture: policy deny when allow expected is a hard fail", () => {
  const o = { ...goodOutcome(), policyAllowed: false };
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
});

// --- scorer — edge cases ---

test("scoreFixture: empty expectations (smoke) scores 1.0, no hard fail", () => {
  const smokeFixture = validFixture({ expectations: {} });
  const s = scoreFixture(smokeFixture, goodOutcome());
  assert.equal(s.score, 1.0);
  assert.equal(s.hardFail, false);
});

test("scoreFixture: no duplicate reply in smoke fixture passes", () => {
  const smokeFixture = validFixture({ expectations: { no_duplicate_reply: true } });
  const s = scoreFixture(smokeFixture, goodOutcome());
  assert.equal(s.score, 1.0);
  assert.equal(s.hardFail, false);
});

test("scoreFixture: policy_verdict deny passes when policy denied", () => {
  const denyFixture = validFixture({
    expectations: { policy_verdict: "deny", reply_operations: [], reply_contains_any: [] },
  });
  const o = { ...goodOutcome(), policyAllowed: false };
  const s = scoreFixture(denyFixture, o);
  assert.equal(s.hardFail, false);
  assert.equal(s.fails, 0);
});

test("scoreFixture: policy_verdict deny fails when policy allowed", () => {
  const denyFixture = validFixture({
    expectations: { policy_verdict: "deny", reply_operations: [], reply_contains_any: [] },
  });
  const s = scoreFixture(denyFixture, goodOutcome());
  assert.equal(s.hardFail, false); // deny-wrong is NOT a hard fail
  assert.ok(s.fails > 0);
});

test("scoreFixture: max_latency_ms passes when under threshold", () => {
  const latFixture = validFixture({ expectations: { max_latency_ms: 5000 } });
  const o = { ...goodOutcome(), latencyMs: 100 };
  const s = scoreFixture(latFixture, o);
  assert.equal(s.hardFail, false);
  assert.equal(s.fails, 0);
});

test("scoreFixture: max_latency_ms fails when over threshold", () => {
  const latFixture = validFixture({ expectations: { max_latency_ms: 50 } });
  const o = { ...goodOutcome(), latencyMs: 500 };
  const s = scoreFixture(latFixture, o);
  assert.equal(s.hardFail, false);
  assert.ok(s.fails > 0);
});

test("scoreFixture: null latency with max_latency_ms expectation fails", () => {
  const latFixture = validFixture({ expectations: { max_latency_ms: 100 } });
  const o = { ...goodOutcome(), latencyMs: null };
  const s = scoreFixture(latFixture, o);
  assert.equal(s.hardFail, false);
  assert.ok(s.fails > 0);
});

test("compareFixture: candidate better than baseline = improve", () => {
  const cand: FixtureScore = { score: 1.0, passes: 5, fails: 0, details: [], hardFail: false };
  const base: FixtureScore = { score: 0.6, passes: 3, fails: 2, details: [], hardFail: false };
  const v = compareFixture(cand, base);
  assert.equal(v.verdict, "improve");
  assert.ok(v.delta > 0);
});

test("compareFixture: candidate hard-fail vs baseline ok = regress", () => {
  const cand: FixtureScore = { score: 1.0, passes: 5, fails: 0, details: [], hardFail: true };
  const base: FixtureScore = { score: 1.0, passes: 5, fails: 0, details: [], hardFail: false };
  assert.equal(compareFixture(cand, base).verdict, "regress");
});

test("compareFixture: equal scores, no hard fail = neutral", () => {
  const s: FixtureScore = { score: 0.8, passes: 4, fails: 1, details: [], hardFail: false };
  assert.equal(compareFixture(s, s).verdict, "neutral");
});

test("compareFixture: both hard fail = neutral (no meaningful delta)", () => {
  const s: FixtureScore = { score: 0.5, passes: 2, fails: 2, details: [], hardFail: true };
  assert.equal(compareFixture(s, s).verdict, "neutral");
});

test("summarize: aggregate regress forces regress verdict", () => {
  const ok: FixtureScore = { score: 1, passes: 1, fails: 0, details: [], hardFail: false };
  const bad: FixtureScore = { score: 0, passes: 0, fails: 1, details: [], hardFail: false };
  const v = compareFixture(bad, ok); // regress, delta -1
  const sum = summarize([v]);
  assert.equal(sum.verdict, "regress");
});

test("summarize: all-neutral = neutral", () => {
  const s: FixtureScore = { score: 1, passes: 1, fails: 0, details: [], hardFail: false };
  const v = compareFixture(s, s);
  assert.equal(summarize([v]).verdict, "neutral");
});

// --- summarize — edge cases ---

test("summarize: empty verdicts = neutral, score 1.0", () => {
  const sum = summarize([]);
  assert.equal(sum.verdict, "neutral");
  assert.equal(sum.candidateScore, 1);
  assert.equal(sum.baselineScore, 1);
});

test("summarize: all improve = improve", () => {
  const base: FixtureScore = { score: 0.5, passes: 1, fails: 1, details: [], hardFail: false };
  const cand: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: false };
  const v = compareFixture(cand, base);
  assert.equal(v.verdict, "improve");
  assert.equal(summarize([v]).verdict, "improve");
});

test("summarize: regress with delta >= 0 is neutral (anyRegress but delta >= 0)", () => {
  // One fixture regressed (candidate hard-fail, baseline ok) but same score.
  const base: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: false };
  const cand: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: true };
  const v = compareFixture(cand, base);
  assert.equal(v.verdict, "regress"); // hard-fail forces regress
  const sum = summarize([v]);
  assert.equal(sum.verdict, "neutral"); // delta >= 0 overrides anyRegress
});

test("summarize: multi-fixture mixed aggregates correctly", () => {
  const base: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: false };
  const candImprove: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: false };
  const candRegress: FixtureScore = { score: 0.0, passes: 0, fails: 2, details: [], hardFail: false };
  const v1 = compareFixture(candImprove, base); // neutral (equal)
  const v2 = compareFixture(candRegress, base); // regress, delta -1
  const sum = summarize([v1, v2]);
  assert.equal(sum.verdict, "regress");
  assert.ok(sum.delta < 0);
});
