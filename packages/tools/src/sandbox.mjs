import path from "node:path";

export function buildToolContext(options = {}) {
  const workspace = path.resolve(options.workspace || process.cwd());
  const cwd = path.resolve(options.cwd || workspace);
  return {
    stateDir: options.stateDir,
    workspace,
    cwd,
    timeoutMs: Number(options.timeoutMs || 10000),
    maxOutputBytes: Number(options.maxOutputBytes || 12000),
    network: options.network || "approval",
  };
}

export function assertInsideWorkspace(workspace, target) {
  const resolved = path.resolve(workspace, target || ".");
  const relative = path.relative(path.resolve(workspace), resolved);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(`Path escapes workspace: ${target}`);
  }
  return resolved;
}

export function capText(value, maxBytes) {
  const text = String(value || "");
  const buffer = Buffer.from(text);
  if (buffer.length <= maxBytes) {
    return { text, truncated: false };
  }
  return {
    text: buffer.subarray(0, maxBytes).toString("utf8"),
    truncated: true,
  };
}
