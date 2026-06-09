import { appendFile, readFile, rename, writeFile } from "node:fs/promises";
import { createEvent } from "./envelope.mjs";
import { ensureStateDir, stateFile } from "./state-path.mjs";

export async function recordRun(stateDir, run) {
  const resolved = await ensureStateDir(stateDir);
  await appendJsonLine(stateFile(resolved, "runs.jsonl"), run);
  return run;
}

export async function appendEvent(stateDir, event) {
  const resolved = await ensureStateDir(stateDir);
  const normalized = event.eventId ? event : createEvent(event.type, event);
  await appendJsonLine(stateFile(resolved, "events.jsonl"), normalized);
  return normalized;
}

export async function updateRunStatus(stateDir, runId, status, patch = {}) {
  const runs = await readRuns(stateDir);
  const index = runs.findLastIndex((run) => run.runId === runId);
  if (index < 0) {
    return null;
  }
  const updated = {
    ...runs[index],
    ...patch,
    status,
    updatedAt: patch.updatedAt || new Date().toISOString(),
  };
  runs[index] = updated;
  await writeJsonLines(stateFile(await ensureStateDir(stateDir), "runs.jsonl"), runs);
  return updated;
}

export async function readRuns(stateDir) {
  return readJsonLines(stateFile(stateDir, "runs.jsonl"));
}

export async function readEvents(stateDir, filter = {}) {
  const events = await readJsonLines(stateFile(stateDir, "events.jsonl"));
  return filter.runId ? events.filter((event) => event.runId === filter.runId) : events;
}

export async function getRun(stateDir, runId) {
  const runs = await readRuns(stateDir);
  return runs.findLast((run) => run.runId === runId) || null;
}

async function appendJsonLine(file, value) {
  await appendFile(file, `${JSON.stringify(value)}\n`, "utf8");
}

async function readJsonLines(file) {
  const text = await readFile(file, "utf8").catch((error) => {
    if (error?.code === "ENOENT") {
      return "";
    }
    throw error;
  });
  return text.split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

async function writeJsonLines(file, values) {
  const tmp = `${file}.tmp`;
  await writeFile(tmp, values.map((value) => JSON.stringify(value)).join("\n") + "\n", "utf8");
  await rename(tmp, file);
}
