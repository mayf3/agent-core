import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

export function loadLocalEnv(options = {}) {
  const cwd = options.cwd || process.cwd();
  const env = options.env || process.env;
  const envPath = options.envPath || join(cwd, ".env");
  if (!existsSync(envPath)) {
    return { loaded: false, path: envPath, keys: [] };
  }
  const keys = [];
  const text = readFileSync(envPath, "utf8");
  for (const line of text.split(/\r?\n/)) {
    const entry = parseEnvLine(line);
    if (!entry || env[entry.key] !== undefined) {
      continue;
    }
    env[entry.key] = entry.value;
    keys.push(entry.key);
  }
  return { loaded: true, path: envPath, keys };
}

export function parseEnvLine(line) {
  const trimmed = line.trim();
  if (!trimmed || trimmed.startsWith("#")) {
    return null;
  }
  const separator = trimmed.indexOf("=");
  if (separator < 1) {
    return null;
  }
  const key = trimmed.slice(0, separator).trim();
  if (!/^[A-Z0-9_]+$/.test(key)) {
    return null;
  }
  return { key, value: parseEnvValue(trimmed.slice(separator + 1).trim()) };
}

function parseEnvValue(value) {
  if (value.startsWith('"') && value.endsWith('"')) {
    try {
      return JSON.parse(value);
    } catch {
      return value.slice(1, -1);
    }
  }
  if (value.startsWith("'") && value.endsWith("'")) {
    return value.slice(1, -1);
  }
  return value;
}
