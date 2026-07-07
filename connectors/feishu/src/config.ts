import { readFileSync, existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface ConnectorConfig {
  appId: string;
  appSecret: string;
  kernelUrl: string;
  kernelIngressTimeoutMs: number;
  connectorPort: number;
  ipcToken: string;
  processingReactionEmoji: string;
  failedReactionEmoji: string;
  reactionStatePath: string;
  reactionRetryAttempts: number;
  reactionRetryBaseDelayMs: number;
  /** Path to JSONL file for execute idempotency persistence (Plan B). */
  executeStatePath: string;
  /** Kernel Decision API base URL for Feishu text approval. */
  kernelDecisionApiUrl: string;
  /** Bearer token for the Kernel Decision API (NEVER share with LLM/harness). */
  kernelDecisionToken: string | undefined;
  /** The Feishu open_id of the authorised approval owner. */
  feishuOwnerOpenId: string | undefined;
}

export function loadConfig(): ConnectorConfig {
  loadLocalEnv();
  const kernelPort = Number(process.env.AGENT_CORE_KERNEL_PORT || 4130);
  const kernelPortStr = String(kernelPort);
  return {
    appId: required("AGENT_CORE_FEISHU_APP_ID"),
    appSecret: required("AGENT_CORE_FEISHU_APP_SECRET"),
    kernelUrl: `http://127.0.0.1:${kernelPort}/v1/ingress`,
    kernelIngressTimeoutMs: Number(process.env.AGENT_CORE_KERNEL_INGRESS_TIMEOUT_MS || 45_000),
    connectorPort: Number(process.env.AGENT_CORE_CONNECTOR_PORT || 4131),
    ipcToken: required("AGENT_CORE_IPC_TOKEN"),
    processingReactionEmoji: optionalReaction("AGENT_CORE_FEISHU_PROCESSING_REACTION", "OK"),
    failedReactionEmoji: optionalReaction("AGENT_CORE_FEISHU_FAILED_REACTION", "ERROR"),
    reactionStatePath: reactionStatePath(),
    reactionRetryAttempts: positiveInt("AGENT_CORE_FEISHU_REACTION_RETRY_ATTEMPTS", 3),
    reactionRetryBaseDelayMs: positiveInt("AGENT_CORE_FEISHU_REACTION_RETRY_BASE_DELAY_MS", 500),
    executeStatePath: executeStatePath(),
    kernelDecisionApiUrl: envString("AGENT_CORE_KERNEL_DECISION_API_URL", `http://127.0.0.1:${kernelPortStr}`),
    kernelDecisionToken: process.env.AGENT_CORE_KERNEL_DECISION_TOKEN
      ? String(process.env.AGENT_CORE_KERNEL_DECISION_TOKEN).trim()
      : undefined,
    feishuOwnerOpenId: process.env.AGENT_CORE_FEISHU_OWNER_OPEN_ID
      ? String(process.env.AGENT_CORE_FEISHU_OWNER_OPEN_ID).trim()
      : undefined,
  };
}

function envString(key: string, fallback: string): string {
  return String(process.env[key] || fallback).trim();
}

function required(key: string): string {
  const value = String(process.env[key] || "").trim();
  if (!value) {
    throw new Error(`${key} is required`);
  }
  return value;
}

function optionalReaction(key: string, fallback: string): string {
  const value = String(process.env[key] ?? fallback).trim().toUpperCase();
  if (value === "0" || value === "FALSE" || value === "OFF" || value === "NONE") {
    return "";
  }
  return value;
}

function positiveInt(key: string, fallback: number): number {
  const raw = Number(process.env[key] ?? fallback);
  if (!Number.isInteger(raw) || raw < 0) {
    return fallback;
  }
  return raw;
}

function reactionStatePath(): string {
  return expandHome(
    process.env.AGENT_CORE_FEISHU_REACTION_STATE_PATH ||
      join(defaultDataDir(), "feishu-reactions.jsonl"),
  );
}

function executeStatePath(): string {
  return expandHome(
    process.env.AGENT_CORE_FEISHU_EXECUTE_STATE_PATH ||
      join(defaultDataDir(), "feishu-executes.jsonl"),
  );
}

function defaultDataDir(): string {
  return expandHome(process.env.AGENT_CORE_DATA_DIR || join(homedir(), ".agent-core"));
}

function loadLocalEnv() {
  if (!existsSync(".env")) {
    return;
  }
  const text = readFileSync(".env", "utf8");
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) {
      continue;
    }
    const index = trimmed.indexOf("=");
    if (index < 1) {
      continue;
    }
    const key = trimmed.slice(0, index).trim();
    if (process.env[key] !== undefined) {
      continue;
    }
    process.env[key] = unquote(trimmed.slice(index + 1).trim());
  }
}

function unquote(value: string): string {
  if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
    return value.slice(1, -1);
  }
  return value;
}

function expandHome(value: string): string {
  if (value === "~") {
    return homedir();
  }
  if (value.startsWith("~/")) {
    return join(homedir(), value.slice(2));
  }
  return value;
}
