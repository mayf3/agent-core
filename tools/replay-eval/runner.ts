/**
 * Replay/Eval runner (Phase 2 replay/eval harness MVP).
 *
 * Drives a Kernel build against a fixture: checks out a git ref into a temp
 * worktree, builds it, starts it on an ephemeral port with an ephemeral DB,
 * POSTs each turn to /v1/ingress, polls /health until terminal, reads the
 * ephemeral DB to extract the observed outcome, then tears everything down.
 *
 * HARD SAFETY:
 * - Ephemeral DB only (fresh temp file); never the production DB.
 * - Ephemeral port; never the operator's running service.
 * - Temporary git worktree; never the operator's working tree.
 * - No .env / secrets / .agent-core / logs reads.
 * - All temp resources are torn down in a finally block.
 */

import { DatabaseSync } from "node:sqlite";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execSync, execFileSync, spawn } from "node:child_process";
import { createServer } from "node:net";
import type { Fixture } from "./fixture.ts";
import type { ReplayOutcome } from "./scorer.ts";

const POLL_INTERVAL_MS = 200;
const POLL_TIMEOUT_MS = 15000;

export interface RunHandle {
  process: ReturnType<typeof spawn>;
  port: number;
  dbPath: string;
  ipcToken: string;
}

/**
 * Structured error for infrastructure failures during replay.
 * Separate from candidate-process crashes — these are driver-level
 * failures (port binding, kernel startup, etc.) that should be
 * reported distinctly in score.json.
 */
export class DriverError extends Error {
  category: string;
  constructor(category: string, message: string) {
    super(message);
    this.name = "DriverError";
    this.category = category;
  }
}

/** Resolve a git ref to a short commit hash. Throws if unresolvable. */
export function resolveRef(ref: string): string {
  try {
    return execFileSync("git", ["rev-parse", "--short", ref], { encoding: "utf8" }).trim();
  } catch {
    throw new Error(`cannot resolve git ref: ${ref}`);
  }
}

/**
 * Build the Kernel binary in the given worktree dir. Returns the binary path.
 * Exported for test injection — tests can replace this to simulate build
 * failures without a real cargo invocation.
 */
export function buildKernel(worktreeDir: string): string {
  try {
    execSync("cargo build --release --bin agent-core-kernel", {
      cwd: worktreeDir,
      stdio: "pipe",
      timeout: 300_000,
    });
  } catch (e) {
    throw new Error(`cargo build failed in ${worktreeDir}: ${(e as Error).message}`);
  }
  return join(worktreeDir, "target", "release", "agent-core-kernel");
}

/** Find a free TCP port by asking the OS. Pure ESM, no require(). */
export async function freePort(): Promise<number> {
  const srv = createServer();
  srv.unref();
  return new Promise<number>((resolve, reject) => {
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      if (addr && typeof addr === "object") {
        const port = addr.port;
        srv.close();
        resolve(port);
      } else {
        reject(new Error("could not bind ephemeral port"));
      }
    });
  });
}

/**
 * Build the minimal environment for a candidate kernel process.
 * Exported for test verification of ambient isolation.
 *
 * The candidate gets PATH, a synthetic HOME pointing to its temporary runtime
 * directory, and stub credentials.  The operator's real HOME, shell config,
 * .env, tokens, and API keys are never inherited.
 *
 * This is ambient-configuration isolation, not a security sandbox.
 * Candidate refs must still be trusted/reviewed until an external sandbox
 * harness exists.  Do not spread process.env.
 */
export function buildCandidateEnv(runtimeDir: string, ipcToken: string): Record<string, string> {
  return {
    PATH: process.env.PATH ?? "",
    HOME: runtimeDir,
    AGENT_CORE_IPC_TOKEN: ipcToken,
    AGENT_CORE_OPENAI_API_KEY: "replay-stub-key",
    AGENT_CORE_FALLBACK_OPENAI_API_KEY: "replay-stub-key",
    AGENT_CORE_MODEL: "local",
    AGENT_CORE_OPENAI_BASE_URL: "http://127.0.0.1:0/v1",
    AGENT_CORE_FALLBACK_OPENAI_BASE_URL: "http://127.0.0.1:0/v1",
    AGENT_CORE_OUTBOX_DISPATCHER_ENABLED: "true",
    AGENT_CORE_CONNECTOR_EXECUTE_URL: "http://127.0.0.1:0/v1/execute",
  };
}

/**
 * Start a Kernel build on an ephemeral port + ephemeral DB. Returns a handle
 * the caller must stop(). The DB is a fresh temp file (never production).
 * Throws DriverError on infrastructure failures.
 */
export async function startKernel(binaryPath: string, ipcToken: string): Promise<RunHandle> {
  const dir = mkdtempSync(join(tmpdir(), "replay-kernel-"));
  const dbPath = join(dir, "ephemeral.db");
  let port: number;
  try {
    port = await freePort();
  } catch (e) {
    rmSync(dir, { recursive: true, force: true });
    throw new DriverError("port_binding", `failed to bind ephemeral port: ${(e as Error).message}`);
  }
  let child: ReturnType<typeof spawn>;
  try {
    child = spawn(binaryPath, ["serve", "--db", dbPath, "--port", String(port)], {
      cwd: dir,
      stdio: "pipe",
      env: buildCandidateEnv(dir, ipcToken),
    });
  } catch (e) {
    rmSync(dir, { recursive: true, force: true });
    throw new DriverError("kernel_startup", `failed to spawn kernel: ${(e as Error).message}`);
  }
  child.on("error", () => {
    try { rmSync(dir, { recursive: true, force: true }); } catch { /* best effort */ }
  });
  return { process: child, port, dbPath, ipcToken };
}

/** Wait for the Kernel's /health endpoint to respond 200. */
export async function waitForReady(handle: RunHandle, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${handle.port}/health`);
      if (res.ok) return;
    } catch {
      // not up yet
    }
    await sleep(200);
  }
  throw new DriverError("kernel_not_ready", `kernel did not become ready on port ${handle.port}`);
}

/** POST a single ingress turn to the Kernel. */
export async function postTurn(handle: RunHandle, fixture: Fixture, turnIndex: number): Promise<void> {
  const turn = fixture.turns[turnIndex];
  const sourceKind = fixture.setup.channel === "feishu" ? "Feishu" : "Cli";
  const envelope = {
    protocol_version: "v1",
    source: sourceKind,
    external_event_id: turn.external_event_id,
    received_at: new Date().toISOString(),
    auth_context: { authenticated: true },
    payload: {
      text: turn.text,
    },
    routing_hint: { session_id: fixture.setup.session_id },
  };
  const res = await fetch(`http://127.0.0.1:${handle.port}/v1/ingress`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${handle.ipcToken}`,
    },
    body: JSON.stringify(envelope),
  });
  if (!res.ok) {
    throw new DriverError("ingress_failed", `/v1/ingress failed (${res.status})`);
  }
}

/** Health snapshot fields the runner consumes. */
interface HealthSnapshot {
  recent_runs_total?: number;
  completed_runs?: number;
  failed_runs?: number;
  worker_jobs?: { pending?: number; running?: number; retryable?: number };
  outbox_dispatches?: { pending?: number; dispatching?: number; retryable?: number };
  ok?: boolean;
}

/**
 * Poll /health until the Kernel has settled for a multi-turn fixture, or
 * timeout.  Settlement means:
 *   - the expected number of turns are represented by worker jobs/runs;
 *   - no worker job is queued/running/retryable;
 *   - no outbox dispatch is pending/dispatching/retryable.
 *
 * Terminal/dead/failed/unknown states are treated as settled evidence, not
 * active work.
 */
export async function pollUntilTerminal(
  handle: RunHandle,
  expectedTurns?: number,
): Promise<{ completed: boolean; latencyMs: number }> {
  const start = Date.now();
  const deadline = start + POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${handle.port}/health`);
      if (!res.ok) {
        await sleep(POLL_INTERVAL_MS);
        continue;
      }
      const health = (await res.json()) as HealthSnapshot;
      const totalRuns = (health.recent_runs_total ?? 0) +
                        (health.completed_runs ?? 0) +
                        (health.failed_runs ?? 0);
      const hasExpectedTurns = expectedTurns
        ? totalRuns >= expectedTurns
        : totalRuns > 0;
      if (!hasExpectedTurns) {
        await sleep(POLL_INTERVAL_MS);
        continue;
      }
      const wj = health.worker_jobs ?? {};
      const od = health.outbox_dispatches ?? {};
      const hasActiveJobs =
        (wj.pending ?? 0) > 0 ||
        (wj.running ?? 0) > 0 ||
        (wj.retryable ?? 0) > 0;
      const hasActiveDispatches =
        (od.pending ?? 0) > 0 ||
        (od.dispatching ?? 0) > 0 ||
        (od.retryable ?? 0) > 0;
      if (!hasActiveJobs && !hasActiveDispatches) {
        return { completed: true, latencyMs: Date.now() - start };
      }
    } catch {
      // transient
    }
    await sleep(POLL_INTERVAL_MS);
  }
  return { completed: false, latencyMs: Date.now() - start };
}

/** Read the ephemeral DB (read-only) to extract the observed outcome. */
export function readOutcome(dbPath: string, fixture: Fixture): ReplayOutcome {
  if (!existsSync(dbPath)) {
    return {
      operations: [],
      replyText: null,
      dispatchCount: 0,
      completed: false,
      latencyMs: null,
      policyAllowed: null,
      crashed: false,
    };
  }
  let db: DatabaseSync;
  try {
    db = new DatabaseSync(dbPath, { readOnly: true });
  } catch {
    return { operations: [], replyText: null, dispatchCount: 0, completed: false, latencyMs: null, policyAllowed: null, crashed: false };
  }
  try {
    const has = (t: string) =>
      (db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name=?").get(t) as { name?: string } | undefined)?.name === t;
    const operations = has("journal_events")
      ? (db.prepare("SELECT DISTINCT json_extract(payload_json,'$.operation') AS op FROM journal_events WHERE op IS NOT NULL").all() as { op?: string }[])
          .map((r) => r.op!)
          .filter(Boolean)
      : [];
    const dispatchCount = has("outbox_dispatches")
      ? (db.prepare("SELECT COUNT(*) AS c FROM outbox_dispatches").get() as { c: number }).c
      : 0;
    const replyText = has("journal_events")
      ? ((db.prepare("SELECT json_extract(payload_json,'$.output.text') AS text FROM journal_events WHERE kind='ReceiptReceived' ORDER BY sequence DESC LIMIT 1").get() as { text?: string } | undefined)?.text ?? null)
      : null;
    const completed = has("runs")
      ? (db.prepare("SELECT COUNT(*) AS c FROM runs WHERE status='Completed'").get() as { c: number }).c > 0
      : false;
    const policyAllowed = has("journal_events")
      ? (db.prepare("SELECT COUNT(*) AS c FROM journal_events WHERE kind='InvocationApproved'").get() as { c: number }).c > 0
      : null;
    return { operations, replyText, dispatchCount, completed, latencyMs: null, policyAllowed, crashed: false };
  } finally {
    db.close();
  }
}

/** Stop the Kernel process + clean up its temp DB dir. */
export function stopKernel(handle: RunHandle): void {
  try {
    handle.process.kill("SIGTERM");
  } catch {
    /* already dead */
  }
  const dir = handle.dbPath ? handle.dbPath.replace(/\/ephemeral\.db$/, "") : null;
  if (dir) {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/**
 * Run one fixture against one build (candidate or baseline). Returns the
 * observed outcome.
 *
 * Infrastructure errors (port binding, kernel not ready, ingress failure)
 * throw DriverError so the caller can distinguish them from candidate-process
 * crashes and report them distinctly in score.json.
 */
export async function runFixtureAgainst(
  binaryPath: string,
  fixture: Fixture,
  ipcToken: string,
): Promise<ReplayOutcome> {
  const handle = await startKernel(binaryPath, ipcToken);
  try {
    await waitForReady(handle);
    for (let i = 0; i < fixture.turns.length; i++) {
      await postTurn(handle, fixture, i);
    }
    await pollUntilTerminal(handle, fixture.turns.length);
    return readOutcome(handle.dbPath, fixture);
  } catch (e) {
    if (e instanceof DriverError) throw e;
    return { ...emptyOutcome(), crashed: true };
  } finally {
    stopKernel(handle);
  }
}

function emptyOutcome(): ReplayOutcome {
  return { operations: [], replyText: null, dispatchCount: 0, completed: false, latencyMs: null, policyAllowed: null, crashed: false };
}
