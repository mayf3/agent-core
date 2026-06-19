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
      stdio: "pipe",
      // Minimal, explicit env only — do NOT spread process.env. Spreading would
      // leak the operator's real secrets (OPENAI_API_KEY, IPC_TOKEN, etc. from
      // .env or shell) into the candidate process, violating the harness's
      // "never touch secrets" boundary. The candidate uses LocalEchoLlm (the
      // stub key below is never a real credential) and a fresh ephemeral DB.
      env: {
        PATH: process.env.PATH ?? "",
        HOME: process.env.HOME ?? "",
        AGENT_CORE_IPC_TOKEN: ipcToken,
        AGENT_CORE_OPENAI_API_KEY: "replay-stub-key",
        AGENT_CORE_FALLBACK_OPENAI_API_KEY: "replay-stub-key",
        AGENT_CORE_MODEL: "local",
        AGENT_CORE_OPENAI_BASE_URL: "http://127.0.0.1:0/v1",
        AGENT_CORE_FALLBACK_OPENAI_BASE_URL: "http://127.0.0.1:0/v1",
        AGENT_CORE_OUTBOX_DISPATCHER_ENABLED: "true",
        AGENT_CORE_CONNECTOR_EXECUTE_URL: "http://127.0.0.1:0/v1/execute",
      },
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

/** Poll /health until the run appears terminal or timeout. */
export async function pollUntilTerminal(handle: RunHandle): Promise<{ completed: boolean; latencyMs: number }> {
  const start = Date.now();
  const deadline = start + POLL_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${handle.port}/health`);
      if (res.ok) {
        const health = (await res.json()) as { recent_runs_total?: number; completed_runs?: number };
        // The Kernel processes synchronously; once health responds the turn is
        // settled. A real MVP would inspect the run status; we approximate
        // completion by the absence of a 5xx and a non-zero run count.
        if ((health.recent_runs_total ?? 0) > 0) {
          return { completed: true, latencyMs: Date.now() - start };
        }
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
    await pollUntilTerminal(handle);
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
