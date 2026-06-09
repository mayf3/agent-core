import { appendFile, readFile, rename, writeFile } from "node:fs/promises";
import { createEvent } from "./envelope.mjs";
import { ensureStateDir, stateFile } from "./state-path.mjs";

export async function recordRun(stateDir, run) {
  const resolved = await ensureStateDir(stateDir);
  await appendStateRecord(resolved, "runs.jsonl", run);
  return run;
}

export async function appendEvent(stateDir, event) {
  const resolved = await ensureStateDir(stateDir);
  const normalized = event.eventId ? event : createEvent(event.type, event);
  await appendStateRecord(resolved, "events.jsonl", normalized);
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
  await writeStateRecords(await ensureStateDir(stateDir), "runs.jsonl", runs);
  return updated;
}

export async function readRuns(stateDir) {
  return readStateRecords(stateDir, "runs.jsonl");
}

export async function readEvents(stateDir, filter = {}) {
  const events = await readStateRecords(stateDir, "events.jsonl");
  return filter.runId ? events.filter((event) => event.runId === filter.runId) : events;
}

export async function getRun(stateDir, runId) {
  const runs = await readRuns(stateDir);
  return runs.findLast((run) => run.runId === runId) || null;
}

export async function appendStateRecord(stateDir, name, value) {
  const resolved = await ensureStateDir(stateDir);
  await appendFile(stateFile(resolved, name), `${JSON.stringify(value)}\n`, "utf8");
}

export async function readStateRecords(stateDir, name) {
  const text = await readFile(stateFile(stateDir, name), "utf8").catch((error) => {
    if (error?.code === "ENOENT") {
      return "";
    }
    throw error;
  });
  return text.split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

export async function writeStateRecords(stateDir, name, values) {
  const file = stateFile(await ensureStateDir(stateDir), name);
  const tmp = `${file}.tmp`;
  await writeFile(tmp, values.map((value) => JSON.stringify(value)).join("\n") + "\n", "utf8");
  await rename(tmp, file);
}
