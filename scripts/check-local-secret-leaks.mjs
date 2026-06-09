import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile } from "node:fs/promises";
import path from "node:path";

const root = process.cwd();
const ignoredFiles = new Set(["pnpm-lock.yaml", "package-lock.json"]);
const findings = [];
const execFileAsync = promisify(execFile);

const patterns = [
  [/sk-[A-Za-z0-9_-]{20,}/, "possible OpenAI-style API key"],
  [/xox[baprs]-[A-Za-z0-9-]{20,}/, "possible Slack token"],
  [/AKIA[0-9A-Z]{16}/, "possible AWS access key"],
  [/-----BEGIN (?:RSA |EC |OPENSSH |)PRIVATE KEY-----/, "private key"],
  [/\b(?:api[_-]?key|app[_-]?secret|access[_-]?token|secret)\s*[:=]\s*['"]?[A-Za-z0-9._~+/\-=]{12,}/i, "possible inline secret"],
  [/\bAuthorization:\s*Bearer\s+[A-Za-z0-9._~+/\-=]{12,}/i, "possible bearer token"],
];

for (const file of await candidateFiles()) {
  await scanFile(path.join(root, file));
}

if (findings.length) {
  console.error("secret scan failed:");
  for (const item of findings) {
    console.error(`- ${item}`);
  }
  process.exit(1);
}

console.log("secret scan passed");

async function scanFile(file) {
  const rel = path.relative(root, file);
  if (ignoredFiles.has(path.basename(file)) || shouldSkip(rel)) {
    return;
  }
  const text = await readFile(file, "utf8").catch(() => "");
  const lines = text.split("\n");
  lines.forEach((line, index) => {
    if (line.includes("example") || line.includes("<personal-repo-url>")) {
      return;
    }
    for (const [pattern, label] of patterns) {
      if (pattern.test(line)) {
        findings.push(`${rel}:${index + 1} ${label}`);
      }
    }
  });
}

function shouldSkip(rel) {
  return /\.(png|jpg|jpeg|gif|webp|ico|pdf|lock)$/i.test(rel);
}

async function candidateFiles() {
  try {
    const { stdout } = await execFileAsync("git", ["ls-files", "--cached", "--others", "--exclude-standard"], { cwd: root });
    return stdout.split("\n").filter(Boolean);
  } catch {
    return [];
  }
}
