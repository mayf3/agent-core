import { randomUUID } from "node:crypto";

export function createRunId(prefix = "run", now = new Date()) {
  const stamp = now.toISOString().replaceAll(/[-:.TZ]/g, "").slice(0, 14);
  return `${prefix}_${stamp}_${randomUUID().slice(0, 8)}`;
}

export function createEvent(type, payload = {}, now = new Date()) {
  return {
    eventId: `evt_${randomUUID()}`,
    type,
    at: now.toISOString(),
    ...payload,
  };
}

export function createRunRecord(input = {}, now = new Date()) {
  const runId = input.runId || createRunId(input.prefix || "run", now);
  return {
    runId,
    source: input.source || "local",
    userId: input.userId || null,
    sessionId: input.sessionId || null,
    status: input.status || "created",
    inputSummary: input.inputSummary || "",
    resultSummary: input.resultSummary || "",
    createdAt: input.createdAt || now.toISOString(),
    updatedAt: input.updatedAt || now.toISOString(),
  };
}

export function okEnvelope({ runId, status = "ok", result = null, events = [], usage = {} } = {}) {
  return {
    ok: true,
    status,
    runId: runId || null,
    result,
    events,
    usage: { elapsedMs: 0, ...usage },
  };
}

export function errorEnvelope({ runId, code, message, recoverable = false, events = [], usage = {} } = {}) {
  return {
    ok: false,
    status: "failed",
    runId: runId || null,
    error: {
      code: code || "unknown_error",
      message: message || "Unknown error.",
      recoverable,
    },
    events,
    usage: { elapsedMs: 0, ...usage },
  };
}
