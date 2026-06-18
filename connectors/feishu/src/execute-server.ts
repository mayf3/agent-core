import http from "node:http";
import type { ConnectorConfig } from "./config.js";
import type { ReactionTracker } from "./reactions.js";
import type { ExecuteStore } from "./execute-store.js";

export function startExecuteServer(
  config: ConnectorConfig,
  client: any,
  reactions?: ReactionTracker,
  executeStore?: ExecuteStore,
) {
  // In-flight dedup within this process (concurrent requests for the same key).
  const inFlight = new Map<string, Promise<unknown>>();
  // Persisted dedup across restarts (Phase 3 connector-local execute idempotency).
  const store = executeStore;
  const server = http.createServer(async (req, res) => {
    try {
      if (req.method !== "POST" || req.url !== "/v1/execute") {
        return json(res, 404, { ok: false, error: "not_found" });
      }
      if (req.headers.authorization !== `Bearer ${config.ipcToken}`) {
        return json(res, 401, { ok: false, error: "unauthorized" });
      }
      const body = await readJson(req);
      validateExecute(body);
      const idempotencyKey = String(body.idempotency_key);

      // 1. In-flight dedup (same process, concurrent).
      const pending = inFlight.get(idempotencyKey);
      if (pending) {
        console.log(`execute replayed (in-flight) idempotency_key=${shortId(idempotencyKey)} invocation=${shortId(body.invocation_id)}`);
        const receipt = await pending;
        return json(res, 200, { ok: true, receipt, replayed: true });
      }

      // 2. Persisted dedup (cross-restart replay). If we already sent this
      //    message successfully, do NOT call sendReply again.
      const stored = store?.get(idempotencyKey);
      if (stored && stored.status === "sent") {
        console.log(`execute replayed (persisted) idempotency_key=${shortId(idempotencyKey)} invocation=${shortId(body.invocation_id)}`);
        const receipt = {
          message_id: stored.receiptSummary?.messageId ?? null,
          status: "sent",
        };
        return json(res, 200, { ok: true, receipt, replayed: true });
      }

      console.log(`execute approved operation=${body.operation} invocation=${shortId(body.invocation_id)} msg=${shortId(body.arguments.message_id)}`);
      const promise = sendReply(client, body.arguments.message_id, body.arguments.text)
        .then((receipt) => {
          void reactions?.markSucceeded(body.arguments.message_id);
          // Persist SUCCESS only — a failure must not be recorded as sent.
          if (store) {
            const now = new Date().toISOString();
            store.set({
              idempotencyKey,
              invocationId: String(body.invocation_id),
              operation: String(body.operation),
              status: "sent",
              receiptSummary: { messageId: receipt.message_id ?? null },
              createdAt: now,
              updatedAt: now,
            });
          }
          return receipt;
        })
        .catch((error) => {
          inFlight.delete(idempotencyKey);
          void reactions?.markFailed(body.arguments.message_id);
          throw error;
        });
      inFlight.set(idempotencyKey, promise);
      const receipt = await promise;
      console.log(`execute sent status=${receipt.status} reply=${shortId(receipt.message_id || "")}`);
      return json(res, 200, { ok: true, receipt });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return json(res, 500, { ok: false, error: message.slice(0, 200) });
    }
  });
  server.listen(config.connectorPort, "127.0.0.1", () => {
    console.log(`feishu connector execute server listening on 127.0.0.1:${config.connectorPort}`);
  });
  return server;
}

export function validateExecute(body: any) {
  if (body.protocol_version !== "v1") {
    throw new Error("unsupported protocol version");
  }
  if (body.operation !== "feishu.send_message") {
    throw new Error("operation_not_allowed");
  }
  if (!body.invocation_id || !body.decision_id || !body.idempotency_key || !body.arguments?.message_id || !body.arguments?.text) {
    throw new Error("invalid execute payload");
  }
}

async function sendReply(client: any, messageId: string, text: string) {
  const response = await client.request({
    method: "POST",
    url: `/open-apis/im/v1/messages/${encodeURIComponent(messageId)}/reply`,
    data: {
      msg_type: "text",
      content: JSON.stringify({ text }),
    },
  });
  return {
    message_id: response?.data?.message_id || response?.data?.message?.message_id || null,
    status: "sent",
  };
}

async function readJson(req: http.IncomingMessage) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(Buffer.from(chunk));
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
}

function json(res: http.ServerResponse, status: number, body: unknown) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  res.end(payload);
}

function shortId(value: string) {
  if (!value) {
    return "-";
  }
  return value.length <= 10 ? value : `${value.slice(0, 4)}...${value.slice(-4)}`;
}
