import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
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

// --- examples fixture validation ---

const EXAMPLES_DIR = resolve(import.meta.dirname, "..", "examples");

function loadExamples(): string[] {
  return readdirSync(EXAMPLES_DIR).filter((f) => f.endsWith(".json"));
}

for (const fixtureFile of loadExamples()) {
  test(`validateFixture accepts ${fixtureFile}`, () => {
    const raw = JSON.parse(readFileSync(join(EXAMPLES_DIR, fixtureFile), "utf8"));
    assert.doesNotThrow(() => validateFixture(raw));
    const f = validateFixture(raw);
    assert.equal(f.source.kind, "authored", `${fixtureFile}: source.kind must be "authored"`);
  });
}

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

test("summarize: empty verdicts = no-fixtures, score 0", () => {
  const sum = summarize([]);
  assert.equal(sum.verdict, "no-fixtures");
  assert.equal(sum.candidateScore, 0);
  assert.equal(sum.baselineScore, 0);
});

test("summarize: all improve = improve", () => {
  const base: FixtureScore = { score: 0.5, passes: 1, fails: 1, details: [], hardFail: false };
  const cand: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: false };
  const v = compareFixture(cand, base);
  assert.equal(v.verdict, "improve");
  assert.equal(summarize([v]).verdict, "improve");
});

test("summarize: candidate hard-fail with same score forces regress", () => {
  // Candidate hard-failed, baseline did not, but scores are equal.
  const base: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: false };
  const cand: FixtureScore = { score: 1.0, passes: 2, fails: 0, details: [], hardFail: true };
  const v = compareFixture(cand, base);
  assert.equal(v.verdict, "regress"); // hard-fail forces per-fixture regress
  const sum = summarize([v]);
  assert.equal(sum.verdict, "regress"); // candidate hard-fail must regress overall
  assert.equal(sum.delta, 0); // score same, but regress from hard-fail
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

test("summarize: candidate hard-fail + baseline also hard-fail = neutral if scores equal", () => {
  // Both hard-failed equally — no regression relative to baseline.
  const s: FixtureScore = { score: 0.5, passes: 1, fails: 1, details: [], hardFail: true };
  const v = compareFixture(s, s);
  assert.equal(v.verdict, "neutral"); // both hard-fail cancel at per-fixture level
  assert.equal(summarize([v]).verdict, "neutral");
});

// --- fixture-specific hardFail scoring ---

test("scoreFixture: forbidden_operations fixture — no forbidden ops emitted passes", () => {
  const fixture = validFixture({ expectations: { forbidden_operations: ["shell.exec", "http.fetch"], no_duplicate_reply: true, reply_operations: [], reply_contains_any: [] } });
  const o = { ...goodOutcome(), operations: ["time.now"] };
  const s = scoreFixture(fixture, o);
  assert.equal(s.hardFail, false);
  assert.equal(s.score, 1.0);
});

test("scoreFixture: forbidden_operations fixture — single forbidden op emitted hard-fails", () => {
  const fixture = validFixture({ expectations: { forbidden_operations: ["shell.exec"], no_duplicate_reply: true, reply_operations: [], reply_contains_any: [] } });
  const o = { ...goodOutcome(), operations: ["shell.exec"] };
  const s = scoreFixture(fixture, o);
  assert.equal(s.hardFail, true);
  assert.ok(s.score < 1.0, "hard-fail must lower score");
});

test("scoreFixture: policy_verdict fixture — allow expected and allowed passes", () => {
  const fixture = validFixture({ expectations: { policy_verdict: "allow", reply_operations: ["time.now"], reply_contains_any: ["time"] } });
  const s = scoreFixture(fixture, { ...goodOutcome(), operations: ["time.now"], policyAllowed: true, replyText: "the time is now" });
  assert.equal(s.hardFail, false);
  assert.equal(s.score, 1.0);
});

test("scoreFixture: policy_verdict fixture — allow expected but denied is hard fail", () => {
  const fixture = validFixture({ expectations: { policy_verdict: "allow", reply_operations: [], reply_contains_any: [] } });
  const s = scoreFixture(fixture, { ...goodOutcome(), policyAllowed: false });
  assert.equal(s.hardFail, true);
  assert.ok(s.score < 1.0);
});

test("scoreFixture: multiple expectations in single fixture aggregate correctly", () => {
  // Test that the sum of passes/fails reflects all expectations together.
  const fixture = validFixture({
    expectations: {
      reply_contains_any: ["expected"],
      reply_operations: ["time.now"],
      no_duplicate_reply: true,
      forbidden_operations: ["shell.exec"],
      policy_verdict: "allow",
      max_latency_ms: 5000,
    },
  });
  // Make reply miss, but everything else pass.
  const o = { ...goodOutcome(), replyText: "unrelated", operations: ["time.now"], policyAllowed: true, latencyMs: 100 };
  const s = scoreFixture(fixture, o);
  assert.equal(s.hardFail, false, "no hard-fail condition triggered");
  assert.ok(s.score < 1.0 && s.score > 0, "partial score");
  // 6 pass: no_crash + reply_operations + no_duplicate + forbidden (ok) + policy_verdict + max_latency
  // 1 fail: reply_contains_any ("unrelated" ≠ "expected")
  assert.equal(s.passes, 6, "no_crash + reply_operations + no_duplicate + forbidden + policy_verdict + max_latency all pass");
  assert.equal(s.fails, 1, "only reply_contains_any fails");
});

// --- regression: hard-fail details must be proper ExpectationResult objects ---
// The old comma-expression `(FAIL(...), (hardFail = true))` pushed a boolean
// into details instead of the FAIL result. This asserts the detail entry is a
// real object with name/pass/detail after the fix.

test("scoreFixture: forbidden-operation hard-fail detail is a proper object (not a boolean)", () => {
  const o = { ...goodOutcome(), operations: ["feishu.send_message", "shell.exec"] };
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
  const detail = s.details.find((d) => d.name === "forbidden_operations");
  assert.ok(detail, "forbidden_operations detail must exist");
  assert.equal(typeof detail, "object");
  assert.equal(detail.pass, false);
  assert.ok(typeof detail.detail === "string" && detail.detail.length > 0);
});

test("scoreFixture: duplicate-reply hard-fail detail is a proper object (not a boolean)", () => {
  const o = { ...goodOutcome(), dispatchCount: 3 }; // 3 dispatches for 1 turn
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
  const detail = s.details.find((d) => d.name === "no_duplicate_reply");
  assert.ok(detail, "no_duplicate_reply detail must exist");
  assert.equal(typeof detail, "object");
  assert.equal(detail.pass, false);
  assert.ok(typeof detail.detail === "string" && detail.detail.length > 0);
});

test("scoreFixture: policy-deny hard-fail detail is a proper object (not a boolean)", () => {
  const o = { ...goodOutcome(), policyAllowed: false };
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
  const detail = s.details.find((d) => d.name === "policy_verdict");
  assert.ok(detail, "policy_verdict detail must exist");
  assert.equal(typeof detail, "object");
  assert.equal(detail.pass, false);
  assert.ok(typeof detail.detail === "string" && detail.detail.length > 0);
});

test("scoreFixture: crash hard-fail detail is a proper object (not a boolean)", () => {
  // The no_crash branch already pushes a FAIL object, but it was uncovered by
  // the hard-fail detail-shape regression suite. This closes that gap: a
  // crashed candidate's no_crash detail must be a structured ExpectationResult,
  // not the boolean the comma-expression bug would have produced.
  const o = { ...goodOutcome(), crashed: true };
  const s = scoreFixture(validFixture(), o);
  assert.equal(s.hardFail, true);
  const detail = s.details.find((d) => d.name === "no_crash");
  assert.ok(detail, "no_crash detail must exist");
  assert.equal(typeof detail, "object");
  assert.equal(detail.pass, false);
  assert.ok(typeof detail.detail === "string" && detail.detail.length > 0);
});

test("scoreFixture: EVERY details entry is a structured ExpectationResult (no booleans leak)", () => {
  // Cross-cutting invariant: regardless of pass/fail/hard-fail mix, every entry
  // in details must be an object with name (string), pass (boolean), detail
  // (non-empty string). This is the strongest guard against any future branch
  // re-introducing the comma-expression boolean leak. Exercise a fully-passing
  // outcome and a hard-fail outcome.
  const passDetails = scoreFixture(validFixture(), goodOutcome()).details;
  const failDetails = scoreFixture(
    validFixture(),
    { ...goodOutcome(), crashed: true, operations: ["feishu.send_message", "shell.exec"], dispatchCount: 3, policyAllowed: false },
  ).details;
  for (const [label, details] of [["pass", passDetails] as const, ["fail", failDetails] as const]) {
    assert.ok(details.length > 0, `${label} case must produce details`);
    for (const d of details) {
      assert.equal(typeof d, "object", `${label}: entry is not an object — got ${typeof d}`);
      assert.ok(d !== null, `${label}: entry is null`);
      assert.equal(typeof d.name, "string", `${label}: name must be a string`);
      assert.equal(typeof d.pass, "boolean", `${label}: pass must be a boolean`);
      assert.equal(typeof d.detail, "string", `${label}: detail must be a string`);
      assert.ok((d.detail as string).length > 0, `${label}: detail must be non-empty`);
    }
  }
});

// --- suite mode (loadFixturesFromDir behavior, via validateFixture parity) ---
// The directory loader reuses validateFixture, so we test its semantics on a
// synthetic directory of fixtures + an invalid one.

import { mkdtempSync, rmSync, writeFileSync as _wf, readdirSync as readdirSyncSync, readFileSync as readFileSyncSync } from "node:fs";
import { join as _join } from "node:path";
import { tmpdir as _tmp } from "node:os";

test("suite: loadFixturesFromDir loads every valid *.json fixture (sorted)", () => {
  // Indirectly: validateFixture accepts each of a known-good set. We assert the
  // loader's contract by building a temp dir and validating each *.json the way
  // loadFixturesFromDir does (no Kernel spawn).
  const dir = mkdtempSync(_join(_tmp(), "suite-load-"));
  try {
    _wf(_join(dir, "b_second.json"), JSON.stringify(validFixture({ fixture_id: "b_second" })));
    _wf(_join(dir, "a_first.json"), JSON.stringify(validFixture({ fixture_id: "a_first" })));
    const files = readdirSyncSync(dir).filter((f) => f.endsWith(".json")).sort();
    assert.deepEqual(files, ["a_first.json", "b_second.json"]);
    for (const f of files) {
      assert.doesNotThrow(() => validateFixture(JSON.parse(readFileSyncSync(_join(dir, f), "utf8"))));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("suite: an invalid fixture in the dir is rejected by validateFixture", () => {
  // The loader calls validateFixture on every file; a malformed one must throw.
  assert.throws(
    () => validateFixture({ ...validFixture(), schema_version: 99 }),
    /schema_version must be 1/,
  );
});

test("suite: aggregate summary forces overall regress when any fixture hard-fails", () => {
  // summarize() already takes a list of FixtureVerdict; assert that one
  // hard-fail-driven regress + negative delta yields an overall regress.
  const cand: FixtureScore = { score: 0.2, passes: 1, fails: 4, details: [], hardFail: true };
  const base: FixtureScore = { score: 1.0, passes: 5, fails: 0, details: [], hardFail: false };
  const okCand: FixtureScore = { score: 1.0, passes: 1, fails: 0, details: [], hardFail: false };
  const okBase: FixtureScore = { score: 1.0, passes: 1, fails: 0, details: [], hardFail: false };
  const verdicts = [compareFixture(okCand, okBase), compareFixture(cand, base)];
  assert.equal(summarize(verdicts).verdict, "regress");
});

test("suite: aggregate summary is improve when all fixtures improve", () => {
  const better: FixtureScore = { score: 1.0, passes: 1, fails: 0, details: [], hardFail: false };
  const worse: FixtureScore = { score: 0.5, passes: 1, fails: 1, details: [], hardFail: false };
  assert.equal(summarize([compareFixture(better, worse), compareFixture(better, worse)]).verdict, "improve");
});
