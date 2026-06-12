import { appendFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
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

export function createJsonlReactionStore(filePath: string): ReactionStateStore {
  return {
    load() {
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
    },
    set(state) {
      appendRecord(filePath, { version: 1, op: "set", state });
    },
    delete(messageId) {
      appendRecord(filePath, {
        version: 1,
        op: "delete",
        messageId,
        updatedAt: new Date().toISOString(),
      });
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
