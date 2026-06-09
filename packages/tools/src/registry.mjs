import { createBuiltinTools } from "./builtins.mjs";

export function createToolRegistry(tools = createBuiltinTools()) {
  const byName = new Map();
  for (const tool of tools) {
    if (byName.has(tool.name)) {
      throw new Error(`Duplicate tool: ${tool.name}`);
    }
    byName.set(tool.name, tool);
  }
  return {
    list() {
      return [...byName.values()].map(({ execute, ...descriptor }) => descriptor);
    },
    get(name) {
      return byName.get(name) || null;
    },
  };
}
