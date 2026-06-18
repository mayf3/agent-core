/**
 * Connector-local execute idempotency persistence (Phase 3).
 *
 * Mirrors reaction-store.ts: a JSONL append-log with load + compact. Survives
 * connector restart so the Kernel can safely replay an `/v1/execute` with the
 * same `idempotency_key` without the connector re-sending the Feishu message.
 *
 * Boundary: records store ONLY minimal fields (idempotency_key, invocation_id,
 * operation, status, timestamps, optional receipt summary). They NEVER store
 * the full Feishu HTTP response, Authorization header, token, secret, or full
 * headers. A TTL/max-age sweep prevents unbounded growth (the old in-memory
 * `Map` grew without bound across a process's lifetime).
 *
 * See docs/decisions/connector-local-durability.md (Plan B).
 */

import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname } from "node:path";

export type ExecuteStatus = "sent" | "failed";

/** A persisted execute-idempotency record. Minimal fields only — no full
 * Feishu response, no Authorization, no token. */
export type StoredExecuteRecord = {
  idempotencyKey: string;
  invocationId: string;
  operation: string;
  status: ExecuteStatus;
  /** Optional, sanitized receipt summary (e.g. the reply message_id). Never
   * the raw Feishu response. */
  receiptSummary?: { messageId: string | null };
  createdAt: string;
  updatedAt: string;
};

export interface ExecuteStore {
  /** Load all non-expired records into a Map keyed by idempotencyKey. */
  load(): Map<string, StoredExecuteRecord>;
  /** Persist a record (append). */
  set(record: StoredExecuteRecord): void;
  /** Look up a single record by idempotencyKey (non-expired only). */
  get(idempotencyKey: string): StoredExecuteRecord | undefined;
}

type StoreRecord =
  | { version: 1; op: "set"; state: StoredExecuteRecord }
  | { version: 1; op: "delete"; idempotencyKey: string; updatedAt: string };

type JsonlStoreOptions = {
  compactAfterBytes?: number;
  /** Max age in ms; records older than this are dropped on load. */
  maxAgeMs?: number;
};

const defaultCompactAfterBytes = 256 * 1024;
const defaultMaxAgeMs = 7 * 24 * 60 * 60 * 1000; // 7 days

export function createJsonlExecuteStore(
  filePath: string,
  options: JsonlStoreOptions = {},
): ExecuteStore {
  const compactAfterBytes = options.compactAfterBytes ?? defaultCompactAfterBytes;
  const maxAgeMs = options.maxAgeMs ?? defaultMaxAgeMs;
  return {
    load() {
      return loadJsonl(filePath, maxAgeMs);
    },
    set(record) {
      appendRecord(filePath, { version: 1, op: "set", state: record });
      compactIfNeeded(filePath, compactAfterBytes, maxAgeMs);
    },
    get(idempotencyKey) {
      return loadJsonl(filePath, maxAgeMs).get(idempotencyKey) ?? undefined;
    },
  };
}

/** In-memory store for tests / ephemeral runs (no disk persistence). */
export function createMemoryExecuteStore(
  initial: StoredExecuteRecord[] = [],
  options: Pick<JsonlStoreOptions, "maxAgeMs"> = {},
): ExecuteStore {
  const maxAgeMs = options.maxAgeMs ?? defaultMaxAgeMs;
  const map = new Map(initial.map((r) => [r.idempotencyKey, r]));
  const sweep = () => {
    const cutoff = Date.now() - maxAgeMs;
    for (const [k, r] of map) {
      if (Date.parse(r.updatedAt) < cutoff) map.delete(k);
    }
  };
  return {
    load() {
      sweep();
      return new Map(map);
    },
    set(record) {
      map.set(record.idempotencyKey, record);
      sweep();
    },
    get(idempotencyKey) {
      sweep();
      const r = map.get(idempotencyKey);
      if (!r) return undefined;
      if (Date.parse(r.updatedAt) < Date.now() - maxAgeMs) {
        map.delete(idempotencyKey);
        return undefined;
      }
      return r;
    },
  };
}

function appendRecord(filePath: string, record: StoreRecord) {
  mkdirSync(dirname(filePath), { recursive: true });
  appendFileSync(filePath, `${JSON.stringify(record)}\n`, "utf8");
}

function loadJsonl(
  filePath: string,
  maxAgeMs: number,
): Map<string, StoredExecuteRecord> {
  const records = new Map<string, StoredExecuteRecord>();
  if (!existsSync(filePath)) {
    return records;
  }
  const cutoff = Date.now() - maxAgeMs;
  const lines = readFileSync(filePath, "utf8").split(/\r?\n/);
  for (const line of lines) {
    const record = parseRecord(line);
    if (!record) continue;
    if (record.op === "delete") {
      records.delete(record.idempotencyKey);
    } else {
      // TTL: drop expired records on load.
      if (Date.parse(record.state.updatedAt) >= cutoff) {
        records.set(record.state.idempotencyKey, record.state);
      }
    }
  }
  return records;
}

function compactIfNeeded(
  filePath: string,
  compactAfterBytes: number,
  maxAgeMs: number,
) {
  if (compactAfterBytes <= 0) return;
  try {
    if (statSync(filePath).size >= compactAfterBytes) {
      compactJsonl(filePath, maxAgeMs);
    }
  } catch {
    // Execute idempotency is connector-local safety metadata; compaction is best-effort.
  }
}

function compactJsonl(filePath: string, maxAgeMs: number) {
  const records = [...loadJsonl(filePath, maxAgeMs).values()];
  const lines = records.map((state) =>
    JSON.stringify({ version: 1, op: "set", state }),
  );
  const text = lines.length > 0 ? `${lines.join("\n")}\n` : "";
  const tempPath = `${filePath}.${process.pid}.tmp`;
  writeFileSync(tempPath, text, "utf8");
  renameSync(tempPath, filePath);
}

function parseRecord(line: string): StoreRecord | null {
  const trimmed = line.trim();
  if (!trimmed) return null;
  try {
    const record = JSON.parse(trimmed) as StoreRecord;
    if (record?.version !== 1) return null;
    if (record.op === "delete" && record.idempotencyKey) return record;
    if (record.op === "set" && isState(record.state)) return record;
  } catch {
    return null;
  }
  return null;
}

function isState(value: unknown): value is StoredExecuteRecord {
  const s = value as StoredExecuteRecord;
  return Boolean(
    s?.idempotencyKey &&
      s.invocationId &&
      s.operation &&
      (s.status === "sent" || s.status === "failed") &&
      s.createdAt &&
      s.updatedAt,
  );
}
