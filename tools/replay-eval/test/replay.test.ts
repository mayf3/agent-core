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

// --- scorer ---

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
