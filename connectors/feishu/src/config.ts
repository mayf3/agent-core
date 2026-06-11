import { readFileSync, existsSync } from "node:fs";

export interface ConnectorConfig {
  appId: string;
  appSecret: string;
  kernelUrl: string;
  kernelIngressTimeoutMs: number;
  connectorPort: number;
  ipcToken: string;
  processingReactionEmoji: string;
  failedReactionEmoji: string;
}

export function loadConfig(): ConnectorConfig {
  loadLocalEnv();
  const kernelPort = Number(process.env.AGENT_CORE_KERNEL_PORT || 4130);
  return {
    appId: required("AGENT_CORE_FEISHU_APP_ID"),
    appSecret: required("AGENT_CORE_FEISHU_APP_SECRET"),
    kernelUrl: `http://127.0.0.1:${kernelPort}/v1/ingress`,
    kernelIngressTimeoutMs: Number(process.env.AGENT_CORE_KERNEL_INGRESS_TIMEOUT_MS || 45_000),
    connectorPort: Number(process.env.AGENT_CORE_CONNECTOR_PORT || 4131),
    ipcToken: required("AGENT_CORE_IPC_TOKEN"),
    processingReactionEmoji: optionalReaction("AGENT_CORE_FEISHU_PROCESSING_REACTION", "OK"),
    failedReactionEmoji: optionalReaction("AGENT_CORE_FEISHU_FAILED_REACTION", "ERROR"),
  };
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
