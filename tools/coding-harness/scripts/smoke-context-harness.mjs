#!/usr/bin/env node
/**
 * smoke-context-harness.mjs — Smoke Runner v0 for context.prepare.v0 Harnesses
 *
 * Validates a local external Harness against its manifest, runs the smoke
 * command, health check, and context.prepare.v0 endpoint check, then
 * outputs a text readiness report with manual registration suggestions.
 *
 * Usage:
 *   node smoke-context-harness.mjs \
 *     --manifest /path/to/harness.manifest.json \
 *     --expect-fragment "EXTERNAL_HARNESS_SCAFFOLD_SMOKE_WORD: papaya"
 *
 * Exit codes:
 *   0 — all checks pass (READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION)
 *   1 — one or more checks failed
 */

import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import http from "node:http";
import { spawnSync } from "node:child_process";

// ── Helpers ──────────────────────────────────────────────────────────────

function jsonParse(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function httpRequest(url, method, body) {
  return new Promise((resolve) => {
    const u = new URL(url);
    const opts = {
      hostname: u.hostname,
      port: u.port,
      path: u.pathname,
      method,
      timeout: 5000,
      headers: body ? { "Content-Type": "application/json" } : undefined,
    };
    const req = http.request(opts, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => {
        const raw = Buffer.concat(chunks).toString("utf-8");
        resolve({ status: res.statusCode ?? 0, body: raw });
      });
    });
    req.on("error", (err) => resolve({ status: 0, body: err.message }));
    req.on("timeout", () => {
      req.destroy();
      resolve({ status: 0, body: "timeout" });
    });
    if (body) req.write(body);
    req.end();
  });
}

function stepResult(passed, details) {
  return { passed, details };
}

/** Human-readable labels for each check step. */
const CHECK_LABELS = {
  manifest_validation:   "manifest schema",
  local_only_check:     "local-only endpoint",
  health_local_check:   "health URL local-only",
  smoke_command:        "smoke command",
  health_check:         "health",
  context_prepare_check: "context.prepare",
};

// ── Argument parsing ─────────────────────────────────────────────────────

function parseArgs() {
  const args = process.argv.slice(2);
  const result = { manifest: null, expectFragment: null };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--manifest":
        result.manifest = args[++i] || null;
        break;
      case "--expect-fragment":
        result.expectFragment = args[++i] || null;
        break;
      case "--help":
      case "-h":
        usage();
        process.exit(0);
      default:
        console.error(`error: unknown argument: ${args[i]}`);
        process.exit(1);
    }
  }
  return result;
}

function usage() {
  const msg = [
    `Usage: smoke-context-harness.mjs --manifest <path> [--expect-fragment <str>]`,
    ``,
    `Validate a local external Harness and output a readiness report.`,
    ``,
    `  --manifest <path>       Path to harness.manifest.json (required)`,
    `  --expect-fragment <str> Expected substring in fragment content`,
    `  --help, -h              Show this message`,
    ``,
    `Exit codes:  0 = all checks pass,  1 = one or more checks failed`,
  ];
  console.log(msg.join("\n"));
}

// ── Manifest validation ──────────────────────────────────────────────────

function validateManifest(m) {
  if (m.schema_version !== "harness-manifest-v0")
    return stepResult(false, `schema_version: expected "harness-manifest-v0", got "${m.schema_version}"`);

  if (typeof m.harness_id !== "string" || m.harness_id.length === 0 || !/^[a-z0-9-]+$/.test(m.harness_id))
    return stepResult(false, "harness_id: invalid or missing");

  if (m.kind !== "context.prepare.v0")
    return stepResult(false, `kind: expected "context.prepare.v0", got "${m.kind}"`);

  if (!m.entrypoint || typeof m.entrypoint.command !== "string" || m.entrypoint.command.length === 0)
    return stepResult(false, "entrypoint.command is missing or empty");

  if (!m.entrypoint || typeof m.entrypoint.cwd !== "string" || m.entrypoint.cwd.length === 0)
    return stepResult(false, "entrypoint.cwd is missing or empty");

  if (!m.health || typeof m.health.url !== "string" || m.health.url.length === 0)
    return stepResult(false, "health.url is missing or empty");

  if (m.health.expected_status !== 200)
    return stepResult(false, `health.expected_status: expected 200, got ${m.health.expected_status}`);

  if (!m.endpoint || typeof m.endpoint.url !== "string" || m.endpoint.url.length === 0)
    return stepResult(false, "endpoint.url is missing or empty");

  if (m.endpoint.local_only !== true)
    return stepResult(false, "endpoint.local_only must be true");

  if (!Array.isArray(m.permissions?.read_paths))
    return stepResult(false, "permissions.read_paths must be an array");

  if (!Array.isArray(m.permissions?.network))
    return stepResult(false, "permissions.network must be an array");

  if (!m.smoke || typeof m.smoke.command !== "string" || m.smoke.command.length === 0)
    return stepResult(false, "smoke.command is missing or empty");

  if (!m.rollback || typeof m.rollback.strategy !== "string" || m.rollback.strategy.length === 0)
    return stepResult(false, "rollback.strategy is missing or empty");

  return stepResult(true, "PASS");
}

// ── Local-only endpoint validation ───────────────────────────────────────

const LOCAL_HOSTS = new Set(["127.0.0.1", "localhost", "::1"]);

function validateLocalOnly(urlStr, label) {
  let u;
  try {
    u = new URL(urlStr);
  } catch {
    return stepResult(false, `${label}: cannot parse URL "${urlStr}"`);
  }

  if (u.protocol !== "http:")
    return stepResult(false, `${label}: protocol must be http, got "${u.protocol}"`);

  if (!LOCAL_HOSTS.has(u.hostname))
    return stepResult(false, `${label}: host must be 127.0.0.1, localhost, or ::1, got "${u.hostname}"`);

  if (!u.port)
    return stepResult(false, `${label}: port is required`);

  if (u.search || u.hash)
    return stepResult(false, `${label}: query string and fragment are forbidden`);

  return stepResult(true, "PASS");
}

// ── Smoke command execution ──────────────────────────────────────────────

function runSmokeCommand(command, cwd) {
  if (!command || typeof command !== "string") return stepResult(false, "empty smoke command");

  const sp = spawnSync(command, [], {
    cwd,
    shell: true,
    timeout: 60000,
    maxBuffer: 1024 * 1024,
  });

  if (sp.error) {
    return stepResult(false, `command error: ${sp.error.message}`);
  }

  if (sp.status !== 0) {
    const allOutput = [sp.stdout?.toString() || "", sp.stderr?.toString() || ""].join("\n").trim();
    return stepResult(false, `exit code ${sp.status}\n${allOutput.slice(0, 600)}`);
  }

  return stepResult(true, "PASS");
}

// ── Health check ─────────────────────────────────────────────────────────

async function checkHealth(healthUrl, expectedStatus) {
  const resp = await httpRequest(healthUrl, "GET");

  if (resp.status === 0) {
    return stepResult(false, `SMOKE_FAILED_HEALTH_UNREACHABLE: ${resp.body}`);
  }
  if (resp.status !== expectedStatus) {
    return stepResult(false, `HTTP ${resp.status}, expected ${expectedStatus}`);
  }

  const body = jsonParse(resp.body);
  if (!body) {
    return stepResult(false, "response is not valid JSON");
  }
  if (body.status !== "ok") {
    return stepResult(false, `status="${body.status}", expected "ok"`);
  }

  return stepResult(true, "PASS");
}

// ── Context.prepare check ────────────────────────────────────────────────

async function checkContextPrepare(endpointUrl, expectFragment) {
  const requestBody = JSON.stringify({
    hook: "context.prepare.v0",
    request_id: `smoke-runner-${Date.now()}`,
    timestamp: new Date().toISOString(),
    payload: {},
  });

  const resp = await httpRequest(endpointUrl, "POST", requestBody);

  if (resp.status === 0) {
    return stepResult(false, `request failed: ${resp.body}`);
  }
  if (resp.status !== 200) {
    return stepResult(false, `HTTP ${resp.status}, expected 200`);
  }

  const body = jsonParse(resp.body);
  if (!body) {
    return stepResult(false, "response is not valid JSON");
  }

  if (body.hook !== "context.prepare.v0") {
    return stepResult(false, `hook="${body.hook}", expected "context.prepare.v0"`);
  }

  let fragments = body.payload?.fragments ?? body.fragments ?? null;
  if (!Array.isArray(fragments)) {
    return stepResult(false, "response contains no fragments array");
  }
  if (fragments.length === 0) {
    return stepResult(false, "fragments array is empty");
  }

  if (expectFragment) {
    const found = fragments.some(
      (f) => typeof f.content === "string" && f.content.includes(expectFragment)
    );
    if (!found) {
      return stepResult(false, `expected smoke word "${expectFragment}" not found`);
    }
  }

  return stepResult(true, "PASS");
}

// ── Manual registration suggestion ───────────────────────────────────────

function formatRegistrationSuggestion(endpointUrl) {
  const env = [
    `- AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true`,
    `- AGENT_CORE_CONTEXT_PREPARE_HOOK_URL=${endpointUrl}`,
    `- AGENT_CORE_CONTEXT_PREPARE_HOOK_FAILURE_MODE=fail_open`,
    `- AGENT_CORE_CONTEXT_PREPARE_HOOK_TIMEOUT_MS=1000`,
  ];

  const rollback = [
    `- restore previous AGENT_CORE_CONTEXT_PREPARE_HOOK_URL`,
    `- or set AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=false`,
  ];

  return { env, rollback };
}

// ── Report output ────────────────────────────────────────────────────────

function outputReport(steps, manifest, endpointUrl, harnessId, expectFragment) {
  const allPassed = Object.values(steps).every((s) => s.passed);

  const lines = [];
  lines.push("HARNESS_SMOKE_RUNNER_REPORT");

  if (!allPassed) {
    lines.push("Result: FAIL");
  }

  lines.push("");
  lines.push("Manifest:");
  lines.push(`- path: ${manifest}`);
  lines.push(`- harness_id: ${harnessId || "unknown"}`);
  lines.push(`- kind: context.prepare.v0`);

  lines.push("");
  lines.push("Checks:");
  for (const [key, label] of Object.entries(CHECK_LABELS)) {
    const step = steps[key];
    if (step) {
      const status = step.passed ? "PASS" : "FAIL";
      lines.push(`- ${label}: ${status}`);
      if (!step.passed) {
        lines.push(`    ${step.details.replace(/\n/g, "\n    ")}`);
      }
    }
  }

  // Smoke word check (derived from context.prepare step + expectFragment).
  if (expectFragment) {
    const cpStep = steps.context_prepare_check;
    const swPassed = cpStep?.passed === true;
    lines.push(`- smoke word: ${swPassed ? "PASS" : "FAIL"}`);
    if (!swPassed && cpStep) {
      lines.push(`    ${cpStep.details.replace(/\n/g, "\n    ")}`);
    }
  }

  lines.push("");

  if (allPassed) {
    lines.push("Readiness:");
    lines.push("- READY_FOR_MANUAL_KERNEL_HOOK_REGISTRATION");
    lines.push("");

    if (endpointUrl) {
      const sug = formatRegistrationSuggestion(endpointUrl);
      lines.push("Suggested manual env:");
      for (const e of sug.env) lines.push(e);
      lines.push("");
      lines.push("Rollback:");
      for (const r of sug.rollback) lines.push(r);
      lines.push("");
    }

    lines.push("This tool did not modify Kernel env.");
    lines.push("This tool did not restart Kernel.");
    lines.push("This tool did not enable the hook.");
    lines.push("Manual approval is required before registration.");
  } else {
    const failedEntry = Object.entries(steps).find(([, s]) => !s.passed);
    if (failedEntry) {
      const label = CHECK_LABELS[failedEntry[0]] || failedEntry[0];
      lines.push(`Failed check: ${label}`);
      lines.push(`Reason: ${failedEntry[1].details.replace(/\n/g, "; ")}`);
    }
    lines.push("");
    lines.push("No Kernel env was modified.");
    lines.push("No hook was enabled.");
  }

  console.log(lines.join("\n"));
  return allPassed ? 0 : 1;
}

// ── Main ─────────────────────────────────────────────────────────────────

async function main() {
  const args = parseArgs();

  if (!args.manifest) {
    console.error("error: --manifest is required");
    usage();
    process.exit(1);
  }

  const absManifest = resolve(args.manifest);
  const manifestDir = dirname(absManifest);
  const steps = {};

  // Read manifest.
  let raw;
  try {
    raw = readFileSync(absManifest, "utf-8");
  } catch (err) {
    console.log("HARNESS_SMOKE_RUNNER_REPORT");
    console.log("Result: FAIL");
    console.log("");
    console.log(`Failed check: manifest schema`);
    console.log(`Reason: cannot read manifest: ${err.message}`);
    console.log("");
    console.log("No Kernel env was modified.");
    console.log("No hook was enabled.");
    process.exit(1);
  }

  const m = jsonParse(raw);
  if (!m) {
    console.log("HARNESS_SMOKE_RUNNER_REPORT");
    console.log("Result: FAIL");
    console.log("");
    console.log("Failed check: manifest schema");
    console.log("Reason: manifest is not valid JSON");
    console.log("");
    console.log("No Kernel env was modified.");
    console.log("No hook was enabled.");
    process.exit(1);
  }

  const harnessId = typeof m.harness_id === "string" ? m.harness_id : null;

  // 1. Manifest validation
  steps.manifest_validation = validateManifest(m);
  if (!steps.manifest_validation.passed) {
    return outputReport(steps, absManifest, null, harnessId, args.expectFragment);
  }

  const endpointUrl = m?.endpoint?.url || null;
  const healthUrl = m?.health?.url || null;

  // 2. Local-only endpoint checks
  if (endpointUrl) {
    steps.local_only_check = validateLocalOnly(endpointUrl, "endpoint.url");
  } else {
    steps.local_only_check = stepResult(false, "endpoint.url: missing");
  }

  if (healthUrl) {
    steps.health_local_check = validateLocalOnly(healthUrl, "health.url");
  } else {
    steps.health_local_check = stepResult(false, "health.url: missing");
  }

  // 3. Smoke command
  if (m?.smoke?.command) {
    steps.smoke_command = runSmokeCommand(m.smoke.command, manifestDir);
  } else {
    steps.smoke_command = stepResult(false, "no smoke.command found");
  }

  // 4. Health check
  if (healthUrl) {
    steps.health_check = await checkHealth(healthUrl, m?.health?.expected_status ?? 200);
  } else {
    steps.health_check = stepResult(false, "no health.url found");
  }

  // 5. Context.prepare check
  if (endpointUrl) {
    steps.context_prepare_check = await checkContextPrepare(endpointUrl, args.expectFragment);
  } else {
    steps.context_prepare_check = stepResult(false, "no endpoint.url found");
  }

  return outputReport(steps, absManifest, endpointUrl, harnessId, args.expectFragment);
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    console.log("HARNESS_SMOKE_RUNNER_REPORT");
    console.log("Result: FAIL");
    console.log("");
    console.log(`Failed check: internal_error`);
    console.log(`Reason: ${err.message}`);
    console.log("");
    console.log("No Kernel env was modified.");
    console.log("No hook was enabled.");
    process.exit(1);
  });
