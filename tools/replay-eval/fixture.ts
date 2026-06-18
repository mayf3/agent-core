/**
 * Replay/Eval fixture validation + types (Phase 2 replay/eval harness MVP).
 *
 * A fixture is a curated conversation + soft expectations (see
 * docs/replay-eval-harness.md §4). This module validates the fixture schema
 * and exposes the typed shape consumed by the scorer.
 */

export interface FixtureTurn {
  role: "user";
  external_event_id: string;
  text: string;
}

export interface FixtureExpectations {
  /** Operations the agent should emit (any subset present passes). */
  reply_operations?: string[];
  /** Reply text must contain at least one of these. */
  reply_contains_any?: string[];
  /** Exactly one dispatch per ingress (no duplicate reply). */
  no_duplicate_reply?: boolean;
  /** Max end-to-end latency in ms. */
  max_latency_ms?: number;
  /** Policy verdict: the intent must be allowed ("allow") or denied ("deny"). */
  policy_verdict?: "allow" | "deny";
  /** Operations that must NEVER appear. */
  forbidden_operations?: string[];
}

export interface Fixture {
  schema_version: number;
  fixture_id: string;
  description: string;
  source: { kind: "audited" | "authored"; run_id?: string; git_revision?: string };
  setup: { agent_id: string; channel: string; session_id: string };
  turns: FixtureTurn[];
  expectations: FixtureExpectations;
}

export const FIXTURE_SCHEMA_VERSION = 1;

/** Validate a parsed JSON object as a fixture. Throws on schema violation. */
export function validateFixture(raw: unknown): Fixture {
  if (typeof raw !== "object" || raw === null) {
    throw new Error("fixture must be a JSON object");
  }
  const f = raw as Record<string, unknown>;
  if (f.schema_version !== FIXTURE_SCHEMA_VERSION) {
    throw new Error(
      `fixture schema_version must be ${FIXTURE_SCHEMA_VERSION}, got ${f.schema_version}`,
    );
  }
  if (typeof f.fixture_id !== "string" || !f.fixture_id) {
    throw new Error("fixture.fixture_id must be a non-empty string");
  }
  if (typeof f.description !== "string") {
    throw new Error("fixture.description must be a string");
  }
  if (typeof f.source !== "object" || f.source === null) {
    throw new Error("fixture.source must be an object");
  }
  const src = f.source as Record<string, unknown>;
  if (src.kind !== "audited" && src.kind !== "authored") {
    throw new Error('fixture.source.kind must be "audited" or "authored"');
  }
  if (typeof f.setup !== "object" || f.setup === null) {
    throw new Error("fixture.setup must be an object");
  }
  const setup = f.setup as Record<string, unknown>;
  if (typeof setup.agent_id !== "string") throw new Error("fixture.setup.agent_id must be a string");
  if (typeof setup.channel !== "string") throw new Error("fixture.setup.channel must be a string");
  if (typeof setup.session_id !== "string") throw new Error("fixture.setup.session_id must be a string");
  if (!Array.isArray(f.turns) || f.turns.length === 0) {
    throw new Error("fixture.turns must be a non-empty array");
  }
  for (const [i, t] of f.turns.entries()) {
    if (typeof t !== "object" || t === null) throw new Error(`fixture.turns[${i}] must be an object`);
    const turn = t as Record<string, unknown>;
    if (turn.role !== "user") throw new Error(`fixture.turns[${i}].role must be "user"`);
    if (typeof turn.external_event_id !== "string") throw new Error(`fixture.turns[${i}].external_event_id must be a string`);
    if (typeof turn.text !== "string") throw new Error(`fixture.turns[${i}].text must be a string`);
  }
  if (typeof f.expectations !== "object" || f.expectations === null) {
    throw new Error("fixture.expectations must be an object (may be empty for smoke fixtures)");
  }
  return f as unknown as Fixture;
}
