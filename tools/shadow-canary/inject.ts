#!/usr/bin/env npx tsx
/**
 * inject.ts — Shadow Canary Entry Point
 *
 * Dispatches to the appropriate scenario based on the variant argument.
 * Each scenario runs the full one-sentence development flow against
 * shadow services through production Connector code paths.
 *
 * Usage:
 *   npx tsx tools/shadow-canary/inject.ts hook-fresh        [--run-id <id>]
 *   npx tsx tools/shadow-canary/inject.ts hook-dirty        [--run-id <id>]
 *   npx tsx tools/shadow-canary/inject.ts invocable-fresh   [--run-id <id>]
 *   npx tsx tools/shadow-canary/inject.ts invocable-dirty   [--run-id <id>]
 *
 * Legacy:
 *   npx tsx tools/shadow-canary/inject.ts fresh    → hook-fresh
 *   npx tsx tools/shadow-canary/inject.ts dirty    → hook-dirty
 *
 * Environment (set by canary-runtime shadow-e2e):
 *   SHADOW_EVIDENCE_DIR, SHADOW_RUN_ID, SHADOW_VARIANT
 *   AGENT_CORE_KERNEL_PORT, AGENT_CORE_CAPABILITY_DECISION_TOKEN, etc.
 */

import * as fs from "node:fs";
import { evidence } from "./evidence.ts";
import { closeServer, config } from "./connector-shadow.ts";

const RUN_ID = process.env.SHADOW_RUN_ID || `shadow_${Date.now()}`;
const VARIANT = (process.argv[2] || process.env.SHADOW_VARIANT || "hook-fresh").toLowerCase();
const EVIDENCE_DIR = process.env.SHADOW_EVIDENCE_DIR || "/tmp/agent-core-shadow-evidence";
const KERNEL_PORT = parseInt(process.env.AGENT_CORE_KERNEL_PORT || "4130", 10);

// ── Map legacy variant names ─────────────────────────────────────────────
function resolveVariant(v: string): string {
  const map: Record<string, string> = {
    "fresh": "hook-fresh",
    "dirty": "hook-dirty",
  };
  return map[v] || v;
}

async function main() {
  const variant = resolveVariant(VARIANT);

  console.log(`\n========================================`);
  console.log(`Shadow Canary Runner`);
  console.log(`  RUN_ID:   ${RUN_ID}`);
  console.log(`  VARIANT:  ${variant}`);
  console.log(`  EVIDENCE: ${EVIDENCE_DIR}`);
  console.log(`========================================\n`);

  fs.mkdirSync(EVIDENCE_DIR, { recursive: true });

  evidence.write("runner-metadata.json", {
    run_id: RUN_ID,
    variant,
    kernel_port: KERNEL_PORT,
    connector_port: config.connectorPort,
    owner_open_id: config.feishuOwnerOpenId,
    started_at: new Date().toISOString(),
  });

  // Wait for connector to be ready
  console.log("Waiting for shadow connector to be ready...");
  await new Promise(r => setTimeout(r, 3_000));

  // Dispatch to the appropriate scenario
  switch (variant) {
    case "hook-fresh": {
      const { runHookFreshShadow } = await import("./scenarios/hook-fresh.ts");
      await runHookFreshShadow();
      break;
    }
    case "hook-dirty": {
      const { runHookDirtyShadow } = await import("./scenarios/hook-dirty.ts");
      await runHookDirtyShadow();
      break;
    }
    case "invocable-fresh": {
      const { runInvocableFreshShadow } = await import("./scenarios/invocable-fresh.ts");
      await runInvocableFreshShadow();
      break;
    }
    case "invocable-dirty": {
      const { runInvocableDirtyShadow } = await import("./scenarios/invocable-dirty.ts");
      await runInvocableDirtyShadow();
      break;
    }
    default:
      console.error(`Unknown variant: ${variant}`);
      evidence.fail("CONFIG", `unknown variant: ${variant}`);
  }

  // Write summary
  evidence.summary();

  // Close the shadow connector HTTP server so the port is released
  closeServer();

  if (evidence.failed) {
    console.error(`\n❌ FAILED at step: ${evidence._firstFailedStep}`);
    process.exit(1);
  }

  console.log(`\n✅ ALL STEPS PASSED`);
}

main().catch((err) => {
  console.error(`\n❌ FATAL: ${err.message}`);
  evidence.fail("FATAL", err.message, { stack: err.stack });
  evidence.summary();
  closeServer();
  process.exit(1);
});
