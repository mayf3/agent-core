#!/usr/bin/env node
import { runAgentTurn } from "../../agent/src/index.mjs";
import { readApprovals, runDoctor } from "../../core/src/index.mjs";
import { createOpenAiCompatibleProvider } from "../../providers/src/index.mjs";
import { createToolRegistry, resumeApproval, runTool } from "../../tools/src/index.mjs";

const args = process.argv.slice(2);
const command = args[0] || "help";

try {
  if (command === "doctor") {
    const envelope = await runDoctor(parseOptions(args.slice(1)));
    writeOutput(envelope, args.includes("--json"));
    process.exit(envelope.ok ? 0 : 1);
  }
  if (command === "ask") {
    const options = parseOptions(args.slice(1));
    const envelope = await runAgentTurn({
      text: options.text || positionalText(args.slice(1)),
      provider: createOpenAiCompatibleProvider(options),
      stateDir: options.stateDir,
      workspace: options.workspace,
      cwd: options.cwd,
      network: options.network,
      timeoutMs: options.timeoutMs,
      maxOutputBytes: options.maxOutputBytes,
    });
    writeOutput(envelope, args.includes("--json"));
    process.exit(envelope.ok ? 0 : 1);
  }
  if (command === "tools") {
    const tools = createToolRegistry().list();
    writeOutput({ ok: true, status: "ok", result: { tools } }, args.includes("--json"));
    process.exit(0);
  }
  if (command === "tool") {
    const toolName = args[1];
    const options = parseOptions(args.slice(2));
    const envelope = await runTool({
      toolName,
      args: toolArgs(options),
      stateDir: options.stateDir,
      workspace: options.workspace,
      cwd: options.cwd,
      network: options.network,
      timeoutMs: options.timeoutMs,
      maxOutputBytes: options.maxOutputBytes,
    });
    writeOutput(envelope, args.includes("--json"));
    process.exit(envelope.ok ? 0 : 1);
  }
  if (command === "resume") {
    const approvalId = args[1];
    const options = parseOptions(args.slice(2));
    const decision = options.reject ? "rejected" : "approved";
    const envelope = await resumeApproval({ approvalId, decision, stateDir: options.stateDir });
    writeOutput(envelope, args.includes("--json"));
    process.exit(envelope.ok ? 0 : 1);
  }
  if (command === "approvals") {
    const options = parseOptions(args.slice(1));
    const approvals = await readApprovals(options.stateDir, options.status ? { status: options.status } : {});
    writeOutput({ ok: true, status: "ok", result: { approvals } }, args.includes("--json"));
    process.exit(0);
  }
  if (command === "help" || command === "--help" || command === "-h") {
    printHelp();
    process.exit(0);
  }
  throw new Error(`Unknown command: ${command}`);
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error(message);
  process.exit(1);
}

function parseOptions(values) {
  const options = {};
  for (let index = 0; index < values.length; index += 1) {
    const item = values[index];
    if (!item.startsWith("--")) {
      continue;
    }
    const key = toCamel(item.slice(2));
    if (key === "json" || key === "approve" || key === "reject") {
      options[key] = true;
      continue;
    }
    options[key] = values[index + 1];
    index += 1;
  }
  return options;
}

function writeOutput(envelope, asJson) {
  if (asJson) {
    console.log(JSON.stringify(envelope, null, 2));
    return;
  }
  if (!envelope.ok) {
    console.log(`${envelope.error.code}: ${envelope.error.message}`);
    return;
  }
  if (envelope.result?.type === "tool-result") {
    console.log(`${envelope.result.toolName}: ${envelope.status}`);
    console.log(JSON.stringify(envelope.result.output, null, 2));
    return;
  }
  if (envelope.result?.type === "agent-answer") {
    console.log(envelope.result.answer);
    return;
  }
  if (envelope.status === "needs_approval") {
    console.log(`approval required: ${envelope.result.approval.approvalId}`);
    console.log(envelope.result.approval.reason);
    return;
  }
  if (envelope.result?.approvals) {
    console.log(JSON.stringify(envelope.result.approvals, null, 2));
    return;
  }
  if (envelope.result?.tools) {
    for (const tool of envelope.result.tools) {
      console.log(`${tool.name} (${tool.permission})`);
    }
    return;
  }
  const checks = envelope.result.checks;
  console.log(`Agent Core doctor: ${envelope.ok ? "ok" : "failed"}`);
  console.log(`node: ${checks.node}`);
  console.log(`cwd: ${checks.cwd}`);
  console.log(`stateDir: ${checks.stateDir}`);
  console.log(`git: ${checks.git.ok ? (checks.git.clean ? "clean" : "dirty") : "unavailable"}`);
}

function printHelp() {
  console.log(`agent-core

Usage:
  agent-core doctor [--json] [--state-dir <path>]
  agent-core ask --text <task> [--json] [--state-dir <path>] [--workspace <path>]
  agent-core tools [--json]
  agent-core tool <name> [--json] [--state-dir <path>] [--workspace <path>] [--key <value>]
  agent-core approvals [--json] [--state-dir <path>] [--status pending]
  agent-core resume <approvalId> --approve|--reject [--json] [--state-dir <path>]
`);
}

function toolArgs(options) {
  const globals = new Set(["json", "stateDir", "workspace", "cwd", "network", "timeoutMs", "maxOutputBytes"]);
  return Object.fromEntries(Object.entries(options).filter(([key]) => !globals.has(key)));
}

function toCamel(value) {
  return value.replaceAll(/-([a-z])/g, (_, char) => char.toUpperCase());
}

function positionalText(values) {
  const parts = [];
  for (let index = 0; index < values.length; index += 1) {
    const item = values[index];
    if (item.startsWith("--")) {
      index += item === "--json" || item === "--approve" || item === "--reject" ? 0 : 1;
      continue;
    }
    parts.push(item);
  }
  return parts.join(" ");
}
