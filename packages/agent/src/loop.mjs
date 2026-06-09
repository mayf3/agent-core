import { randomUUID } from "node:crypto";
import {
  appendEvent,
  appendStateRecord,
  createEvent,
  createRunRecord,
  errorEnvelope,
  okEnvelope,
  recordRun,
  updateRunStatus,
} from "../../core/src/index.mjs";
import { createToolRegistry, runTool } from "../../tools/src/index.mjs";
import { defaultSystemPrompt } from "./prompt.mjs";

export async function runAgentTurn(input = {}) {
  const started = Date.now();
  const provider = input.provider;
  if (!provider?.generate) {
    return errorEnvelope({ code: "provider_required", message: "Agent turn requires a provider." });
  }
  const text = String(input.text || "").trim();
  if (!text) {
    return errorEnvelope({ code: "input_required", message: "Agent turn requires text." });
  }

  const registry = input.registry || createToolRegistry();
  const run = createRunRecord({
    prefix: "agent",
    source: input.source || "cli",
    sessionId: input.sessionId || null,
    status: "running",
    inputSummary: summarize(text),
  });
  await recordRun(input.stateDir, run);
  await appendEvent(input.stateDir, createEvent("run.started", { runId: run.runId, source: run.source }));
  await appendEvent(input.stateDir, createEvent("agent.turn.started", { runId: run.runId }));

  const messages = [
    { role: "system", content: input.systemPrompt || defaultSystemPrompt() },
    { role: "user", content: text },
  ];
  await recordMessage(input.stateDir, run.runId, messages[1]);

  const maxIterations = Number(input.maxIterations || 6);
  for (let iteration = 0; iteration < maxIterations; iteration += 1) {
  const model = await callModel(provider, input.stateDir, run.runId, messages, registry.list());
    if (!model.ok) {
      await updateRunStatus(input.stateDir, run.runId, "failed", { resultSummary: model.error.message });
      return errorEnvelope({
        runId: run.runId,
        code: model.error.code,
        message: model.error.message,
        recoverable: model.status === "needs_config",
        usage: elapsed(started),
      });
    }

    if (!model.toolCalls.length) {
      await recordMessage(input.stateDir, run.runId, { role: "assistant", content: model.text || "" });
      await appendEvent(input.stateDir, createEvent("agent.turn.completed", { runId: run.runId }));
      await updateRunStatus(input.stateDir, run.runId, "ok", { resultSummary: summarize(model.text || "") });
      return okEnvelope({
        runId: run.runId,
        result: { type: "agent-answer", answer: model.text || "", toolCalls: [] },
        usage: { ...elapsed(started), ...model.usage },
      });
    }

    const assistantToolMessage = assistantToolCallMessage(model);
    messages.push(assistantToolMessage);
    await recordMessage(input.stateDir, run.runId, assistantToolMessage);
    const toolResult = await runOneToolCall(input, run.runId, model.toolCalls[0], registry);
    if (toolResult.status === "needs_approval") {
      return okEnvelope({
        runId: run.runId,
        status: "needs_approval",
        result: { ...toolResult.result, type: "agent-needs-approval" },
        events: toolResult.events,
        usage: elapsed(started),
      });
    }
    if (!toolResult.ok) {
      await updateRunStatus(input.stateDir, run.runId, "failed", { resultSummary: toolResult.error.message });
      return toolResult;
    }
    messages.push({
      role: "tool",
      content: JSON.stringify(toolResult.result.output),
      tool_call_id: model.toolCalls[0].id || model.toolCalls[0].name,
    });
    await recordMessage(input.stateDir, run.runId, messages.at(-1));
  }

  await updateRunStatus(input.stateDir, run.runId, "failed", { resultSummary: "iteration limit reached" });
  return errorEnvelope({ runId: run.runId, code: "agent_iteration_limit", message: "Agent iteration limit reached.", usage: elapsed(started) });
}

function assistantToolCallMessage(model) {
  return {
    role: "assistant",
    content: model.text || "",
    tool_calls: model.toolCalls.map((call) => ({
      id: call.id || call.name,
      type: "function",
      function: { name: call.name, arguments: JSON.stringify(call.args || {}) },
    })),
  };
}

async function callModel(provider, stateDir, runId, messages, tools) {
  const modelCallId = `mdl_${randomUUID()}`;
  await appendStateRecord(stateDir, "context_snapshots.jsonl", {
    runId,
    modelCallId,
    at: new Date().toISOString(),
    messageCount: messages.length,
    toolNames: tools.map((tool) => tool.name),
    contextPolicyVersion: "context.v1",
  });
  await appendEvent(stateDir, createEvent("model.called", { runId, provider: provider.name, model: provider.model || null }));
  const result = await provider.generate({ messages, tools });
  await appendStateRecord(stateDir, "model_calls.jsonl", {
    modelCallId,
    runId,
    provider: provider.name,
    model: result.model || provider.model || null,
    status: result.ok ? "ok" : "failed",
    errorCode: result.error?.code || null,
    toolCallCount: result.toolCalls?.length || 0,
    usage: result.usage || {},
  });
  await appendEvent(stateDir, createEvent(result.ok ? "model.completed" : "model.failed", {
    runId,
    provider: provider.name,
    model: result.model || provider.model || null,
    code: result.error?.code || null,
  }));
  return { ...result, toolCalls: result.toolCalls || [] };
}

async function runOneToolCall(input, runId, toolCall, registry) {
  return runTool({
    toolName: toolCall.name,
    args: toolCall.args || {},
    stateDir: input.stateDir,
    workspace: input.workspace,
    cwd: input.cwd,
    network: input.network,
    timeoutMs: input.timeoutMs,
    maxOutputBytes: input.maxOutputBytes,
    runId,
    registry,
    manageRunStatus: false,
  });
}

async function recordMessage(stateDir, runId, message) {
  await appendStateRecord(stateDir, "messages.jsonl", { runId, at: new Date().toISOString(), ...message });
}

function summarize(value) {
  const text = String(value || "").replaceAll(/\s+/g, " ").trim();
  return text.length > 160 ? `${text.slice(0, 157)}...` : text;
}

function elapsed(started) {
  return { elapsedMs: Date.now() - started };
}
