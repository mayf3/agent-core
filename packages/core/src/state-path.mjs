import { mkdir } from "node:fs/promises";
import path from "node:path";

export function resolveStateDir(options = {}) {
  const cwd = options.cwd || process.cwd();
  const raw = options.stateDir || process.env.AGENT_CORE_STATE_DIR || path.join(cwd, ".agent-core", "state");
  return path.resolve(String(raw));
}

export async function ensureStateDir(stateDir) {
  const resolved = resolveStateDir({ stateDir });
  await mkdir(resolved, { recursive: true });
  return resolved;
}

export function stateFile(stateDir, name) {
  return path.join(resolveStateDir({ stateDir }), name);
}
