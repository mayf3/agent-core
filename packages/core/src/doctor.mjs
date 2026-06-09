import { access } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { okEnvelope } from "./envelope.mjs";
import { ensureStateDir, resolveStateDir } from "./state-path.mjs";

const execFileAsync = promisify(execFile);

export async function runDoctor(options = {}) {
  const started = Date.now();
  const cwd = options.cwd || process.cwd();
  const stateDir = await ensureStateDir(options.stateDir || resolveStateDir({ cwd }));
  const checks = {
    node: process.version,
    cwd,
    stateDir,
    stateWritable: await canAccess(stateDir),
    git: await gitStatus(cwd),
    constraints: {
      maxFileLines: 500,
      maxFilesPerDirectory: 20,
      maxGeneralDepth: 4,
      maxNodeWorkspaceDepth: 6,
    },
  };
  return okEnvelope({
    result: {
      type: "doctor",
      checks,
    },
    usage: { elapsedMs: Date.now() - started },
  });
}

async function canAccess(target) {
  try {
    await access(target);
    return true;
  } catch {
    return false;
  }
}

async function gitStatus(cwd) {
  try {
    const { stdout } = await execFileAsync("git", ["status", "--short"], { cwd });
    return { ok: true, clean: stdout.trim().length === 0 };
  } catch (error) {
    return { ok: false, message: error instanceof Error ? error.message : String(error) };
  }
}
