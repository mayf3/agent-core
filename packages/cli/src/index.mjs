#!/usr/bin/env node
import { runDoctor } from "../../core/src/index.mjs";

const args = process.argv.slice(2);
const command = args[0] || "help";

try {
  if (command === "doctor") {
    const envelope = await runDoctor(parseOptions(args.slice(1)));
    writeOutput(envelope, args.includes("--json"));
    process.exit(envelope.ok ? 0 : 1);
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
    if (values[index] === "--state-dir") {
      options.stateDir = values[index + 1];
      index += 1;
    }
  }
  return options;
}

function writeOutput(envelope, asJson) {
  if (asJson) {
    console.log(JSON.stringify(envelope, null, 2));
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
`);
}
