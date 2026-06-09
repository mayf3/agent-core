const defaultBaseUrl = "https://api.openai.com/v1";

export function buildOpenAiCompatibleConfig(options = {}) {
  const env = options.env || process.env;
  return {
    provider: "openai-compatible",
    apiKey: String(options.apiKey || env.AGENT_CORE_OPENAI_API_KEY || env.OPENAI_API_KEY || "").trim(),
    baseUrl: String(options.baseUrl || env.AGENT_CORE_OPENAI_BASE_URL || env.OPENAI_BASE_URL || defaultBaseUrl).replace(/\/$/, ""),
    model: String(options.model || env.AGENT_CORE_MODEL || env.OPENAI_MODEL || "").trim(),
    timeoutMs: Number(options.timeoutMs || env.AGENT_CORE_MODEL_TIMEOUT_MS || 30000),
  };
}

export function createOpenAiCompatibleProvider(options = {}) {
  const config = buildOpenAiCompatibleConfig(options);
  return {
    name: config.provider,
    model: config.model,
    async generate(input = {}) {
      return generateOpenAiCompatible({ ...input, config, fetchImpl: options.fetchImpl || fetch });
    },
  };
}

async function generateOpenAiCompatible({ messages = [], tools = [], config, fetchImpl }) {
  if (!config.apiKey || !config.model) {
    return {
      ok: false,
      status: "needs_config",
      error: { code: "model_config_required", message: "Set OPENAI_API_KEY and AGENT_CORE_MODEL before asking the agent." },
      provider: config.provider,
      model: config.model || null,
    };
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), config.timeoutMs);
  try {
    const response = await fetchImpl(`${config.baseUrl}/chat/completions`, {
      method: "POST",
      signal: controller.signal,
      headers: {
        authorization: `Bearer ${config.apiKey}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        model: config.model,
        messages,
        tools: tools.length ? tools.map(toOpenAiTool) : undefined,
        tool_choice: tools.length ? "auto" : undefined,
      }),
    });
    const body = await readJson(response);
    if (!response.ok) {
      return {
        ok: false,
        status: "failed",
        error: normalizeError(body, response.status),
        provider: config.provider,
        model: config.model,
      };
    }
    return normalizeChatCompletion(body, config);
  } catch (error) {
    return {
      ok: false,
      status: "failed",
      error: {
        code: error?.name === "AbortError" ? "model_timeout" : "model_request_failed",
        message: error instanceof Error ? error.message : String(error),
      },
      provider: config.provider,
      model: config.model,
    };
  } finally {
    clearTimeout(timer);
  }
}

function normalizeChatCompletion(body, config) {
  const message = body?.choices?.[0]?.message || {};
  return {
    ok: true,
    provider: config.provider,
    model: body.model || config.model,
    text: message.content || "",
    toolCalls: (message.tool_calls || []).map((call) => ({
      id: call.id || null,
      name: call.function?.name || "",
      args: parseArgs(call.function?.arguments),
    })).filter((call) => call.name),
    usage: {
      inputTokens: body.usage?.prompt_tokens ?? null,
      outputTokens: body.usage?.completion_tokens ?? null,
      totalTokens: body.usage?.total_tokens ?? null,
    },
  };
}

function toOpenAiTool(tool) {
  return {
    type: "function",
    function: {
      name: tool.name,
      description: tool.description,
      parameters: tool.inputSchema || { type: "object", additionalProperties: true },
    },
  };
}

function parseArgs(value) {
  try {
    return value ? JSON.parse(value) : {};
  } catch {
    return {};
  }
}

async function readJson(response) {
  try {
    return await response.json();
  } catch {
    return {};
  }
}

function normalizeError(body, status) {
  const error = body?.error || {};
  return {
    code: error.code || error.type || `model_http_${status}`,
    message: error.message || `Model request failed with HTTP ${status}.`,
  };
}
