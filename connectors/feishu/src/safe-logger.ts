const REDACTIONS: Array<[RegExp, string]> = [
  [/Bearer\s+[A-Za-z0-9._-]+/g, "Bearer <redacted>"],
  [/(Authorization\s*:\s*)[^\r\n]+/gi, "$1<redacted>"],
  [/(appSecret["'\s:=]+)[^"',\s}]+/gi, "$1<redacted>"],
  [/(tenant_access_token["'\s:=]+)[^"',\s}]+/gi, "$1<redacted>"],
];

export const safeLarkLogger = {
  error: (...values: unknown[]) => console.error(format("error", values)),
  warn: (...values: unknown[]) => console.warn(format("warn", values)),
  info: (...values: unknown[]) => console.log(format("info", values)),
  debug: (...values: unknown[]) => console.log(format("debug", values)),
  trace: (...values: unknown[]) => console.log(format("trace", values)),
};

function format(level: string, values: unknown[]) {
  return `[lark:${level}] ${values.map(formatValue).join(" ")}`.slice(0, 800);
}

function formatValue(value: unknown): string {
  if (typeof value === "string") {
    return redact(value);
  }
  if (value instanceof Error) {
    return `${value.name}: ${redact(value.message)}`;
  }
  if (typeof value === "number" || typeof value === "boolean" || value === null) {
    return String(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(formatValue).join(", ")}]`;
  }
  if (typeof value === "object") {
    const name = value?.constructor?.name || "Object";
    return `[${name}]`;
  }
  return typeof value;
}

function redact(text: string) {
  return REDACTIONS.reduce((current, [pattern, replacement]) => current.replace(pattern, replacement), text);
}
