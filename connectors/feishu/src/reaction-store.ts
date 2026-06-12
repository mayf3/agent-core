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

export type StoredReactionState = {
  messageId: string;
  reactionId: string;
  emojiType: string;
  status: "processing" | "failed";
  updatedAt: string;
};

export interface ReactionStateStore {
  load(): Map<string, StoredReactionState>;
  set(state: StoredReactionState): void;
  delete(messageId: string): void;
}

type StoreRecord =
  | { version: 1; op: "set"; state: StoredReactionState }
  | { version: 1; op: "delete"; messageId: string; updatedAt: string };

type JsonlStoreOptions = {
  compactAfterBytes?: number;
};

const defaultCompactAfterBytes = 256 * 1024;

export function createJsonlReactionStore(
  filePath: string,
  options: JsonlStoreOptions = {},
): ReactionStateStore {
  const compactAfterBytes = options.compactAfterBytes ?? defaultCompactAfterBytes;
  return {
    load() {
      return loadJsonl(filePath);
    },
    set(state) {
      appendRecord(filePath, { version: 1, op: "set", state });
      compactIfNeeded(filePath, compactAfterBytes);
    },
    delete(messageId) {
      appendRecord(filePath, {
        version: 1,
        op: "delete",
        messageId,
        updatedAt: new Date().toISOString(),
      });
      compactIfNeeded(filePath, compactAfterBytes);
    },
  };
}

export function createMemoryReactionStore(initial: StoredReactionState[] = []): ReactionStateStore {
  const states = new Map(initial.map((state) => [state.messageId, state]));
  return {
    load() {
      return new Map(states);
    },
    set(state) {
      states.set(state.messageId, state);
    },
    delete(messageId) {
      states.delete(messageId);
    },
  };
}

function appendRecord(filePath: string, record: StoreRecord) {
  mkdirSync(dirname(filePath), { recursive: true });
  appendFileSync(filePath, `${JSON.stringify(record)}\n`, "utf8");
}

function loadJsonl(filePath: string): Map<string, StoredReactionState> {
  const states = new Map<string, StoredReactionState>();
  if (!existsSync(filePath)) {
    return states;
  }
  const lines = readFileSync(filePath, "utf8").split(/\r?\n/);
  for (const line of lines) {
    const record = parseRecord(line);
    if (!record) {
      continue;
    }
    if (record.op === "delete") {
      states.delete(record.messageId);
    } else {
      states.set(record.state.messageId, record.state);
    }
  }
  return states;
}

function compactIfNeeded(filePath: string, compactAfterBytes: number) {
  if (compactAfterBytes <= 0) {
    return;
  }
  try {
    if (statSync(filePath).size >= compactAfterBytes) {
      compactJsonl(filePath);
    }
  } catch {
    // Reaction state is connector-local UX metadata; compaction failure is best-effort.
  }
}

function compactJsonl(filePath: string) {
  const states = [...loadJsonl(filePath).values()];
  const records = states.map((state) => JSON.stringify({ version: 1, op: "set", state }));
  const text = records.length > 0 ? `${records.join("\n")}\n` : "";
  const tempPath = `${filePath}.${process.pid}.tmp`;
  writeFileSync(tempPath, text, "utf8");
  renameSync(tempPath, filePath);
}

function parseRecord(line: string): StoreRecord | null {
  const trimmed = line.trim();
  if (!trimmed) {
    return null;
  }
  try {
    const record = JSON.parse(trimmed) as StoreRecord;
    if (record?.version !== 1) {
      return null;
    }
    if (record.op === "delete" && record.messageId) {
      return record;
    }
    if (record.op === "set" && isState(record.state)) {
      return record;
    }
  } catch {
    return null;
  }
  return null;
}

function isState(value: unknown): value is StoredReactionState {
  const state = value as StoredReactionState;
  return Boolean(
    state?.messageId &&
      state.reactionId &&
      state.emojiType &&
      (state.status === "processing" || state.status === "failed") &&
      state.updatedAt,
  );
}
