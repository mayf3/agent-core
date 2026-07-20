//! Evidence collection for Shadow Canary tests.
//!
//! Fail-closed: exits non-zero at FIRST failure with evidence saved.

import * as fs from "node:fs";
import * as path from "node:path";

const EVIDENCE_DIR = process.env.SHADOW_EVIDENCE_DIR || "/tmp/agent-core-shadow-evidence";

export const evidence = {
  _failed: false,
  _firstFailedStep: null as string | null,
  _steps: [] as Array<{ step: string; detail: string; data?: any; passed: boolean }>,

  pass(step: string, detail: string, data?: any) {
    this._steps.push({ step, detail, data, passed: true });
    console.log(`  ✅ ${step}: ${detail}`);
  },

  fail(step: string, detail: string, data?: any) {
    this._steps.push({ step, detail, data, passed: false });
    this._failed = true;
    if (!this._firstFailedStep) this._firstFailedStep = step;
    console.log(`  ❌ ${step}: ${detail}`);
  },

  get failed(): boolean { return this._failed; },

  write(name: string, data: any) {
    const filePath = path.join(EVIDENCE_DIR, name);
    fs.mkdirSync(path.dirname(filePath), { recursive: true });
    fs.writeFileSync(filePath, JSON.stringify(data, null, 2));
  },

  summary() {
    const passed = this._steps.filter(s => s.passed).length;
    const failed = this._steps.filter(s => !s.passed).length;
    const summary = {
      total: this._steps.length, passed, failed,
      first_failed_step: this._firstFailedStep || null,
      steps: this._steps,
    };
    this.write("evidence.json", summary);
    console.log(`\n📊 Evidence summary: ${passed} passed, ${failed} failed`);
  },
};
