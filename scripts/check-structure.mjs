import { readdir, readFile } from "node:fs/promises";
import path from "node:path";

const root = process.cwd();
const maxLines = 500;
const maxFilesPerDir = 20;
const maxDepth = 6;
const ignored = new Set([".git", "node_modules", "dist", "build", "coverage", "target", ".agent-core"]);
const generated = new Set([]);
const failures = [];

await walk(root, 0);

if (failures.length) {
  console.error(failures.join("\n"));
  process.exit(1);
}

console.log("structure check passed");

async function walk(dir, depth) {
  const rel = path.relative(root, dir) || ".";
  if (depth > maxDepth) {
    failures.push(`directory depth exceeds ${maxDepth}: ${rel}`);
    return;
  }

  const entries = await readdir(dir, { withFileTypes: true });
  const visible = entries.filter((entry) => !ignored.has(entry.name));
  const files = visible.filter((entry) => entry.isFile());
  if (files.length > maxFilesPerDir) {
    failures.push(`directory has ${files.length} files, max ${maxFilesPerDir}: ${rel}`);
  }

  for (const entry of visible) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      await walk(full, depth + 1);
      continue;
    }
    if (entry.isFile()) {
      await checkLines(full);
    }
  }
}

async function checkLines(file) {
  const rel = path.relative(root, file);
  if (generated.has(rel) || shouldSkipLineCheck(rel)) {
    return;
  }
  const text = await readFile(file, "utf8");
  const lines = text.split("\n").length;
  if (lines > maxLines) {
    failures.push(`file has ${lines} lines, max ${maxLines}: ${rel}`);
  }
}

function shouldSkipLineCheck(rel) {
  return /\.(png|jpg|jpeg|gif|webp|ico|pdf|lock)$/i.test(rel);
}
