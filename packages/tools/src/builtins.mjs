import { exec } from "node:child_process";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { promisify } from "node:util";
import { readEvents, readRuns } from "../../core/src/index.mjs";
import { assertInsideWorkspace, capText } from "./sandbox.mjs";

const execAsync = promisify(exec);

export function createBuiltinTools() {
  return [
    {
      name: "fs.list",
      description: "List files in the workspace.",
      permission: "read",
      async execute(args, context) {
        const target = assertInsideWorkspace(context.workspace, args.path || ".");
        return { entries: await readdir(target) };
      },
    },
    {
      name: "fs.read",
      description: "Read a text file in the workspace.",
      permission: "read",
      async execute(args, context) {
        const target = assertInsideWorkspace(context.workspace, args.path);
        const capped = capText(await readFile(target, "utf8"), context.maxOutputBytes);
        return { content: capped.text, truncated: capped.truncated };
      },
    },
    {
      name: "fs.write",
      description: "Write a text file in the workspace.",
      permission: "write",
      async execute(args, context) {
        const target = assertInsideWorkspace(context.workspace, args.path);
        await writeFile(target, String(args.content || ""), "utf8");
        return { path: target, bytes: Buffer.byteLength(String(args.content || "")) };
      },
    },
    {
      name: "search.grep",
      description: "Search text files with ripgrep.",
      permission: "read",
      async execute(args, context) {
        const query = String(args.query || "");
        const { stdout } = await execAsync(`rg --line-number -- ${shellQuote(query)} ${shellQuote(context.workspace)}`, {
          cwd: context.cwd,
          timeout: context.timeoutMs,
          maxBuffer: context.maxOutputBytes,
        }).catch((error) => ({ stdout: error.stdout || "" }));
        const capped = capText(stdout, context.maxOutputBytes);
        return { output: capped.text, truncated: capped.truncated };
      },
    },
    {
      name: "shell.exec",
      description: "Run a shell command inside the workspace.",
      permission: "execute",
      async execute(args, context) {
        const { stdout, stderr } = await execAsync(String(args.cmd || ""), {
          cwd: context.cwd,
          timeout: context.timeoutMs,
          maxBuffer: context.maxOutputBytes,
        });
        return {
          stdout: capText(stdout, context.maxOutputBytes).text,
          stderr: capText(stderr, context.maxOutputBytes).text,
        };
      },
    },
    {
      name: "http.fetch",
      description: "Fetch a URL with the runtime fetch API.",
      permission: "execute",
      async execute(args, context) {
        const response = await fetch(String(args.url || ""));
        const capped = capText(await response.text(), context.maxOutputBytes);
        return { status: response.status, body: capped.text, truncated: capped.truncated };
      },
    },
    {
      name: "state.read",
      description: "Read local run and event state.",
      permission: "read",
      async execute(args, context) {
        const runId = args.runId ? String(args.runId) : null;
        return {
          runs: await readRuns(context.stateDir),
          events: await readEvents(context.stateDir, runId ? { runId } : {}),
        };
      },
    },
  ];
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}
