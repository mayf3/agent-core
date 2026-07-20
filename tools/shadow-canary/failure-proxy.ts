#!/usr/bin/env npx tsx
/**
 * failure-proxy.ts — Shadow Failure Proxy (Node.js implementation)
 *
 * Sits between Kernel and Deployment Harness:
 *   Kernel -> Failure Proxy (:7400) -> Deployment Harness (:7401)
 *
 * On the first POST /v1/deployments, returns a definitive rejection.
 * Non-deploy requests (health, disable, rollback, status) are always forwarded.
 */

import * as http from "node:http";

const PROXY_PORT = parseInt(process.env.PROXY_PORT || "7400", 10);
const HARNESS_PORT = parseInt(process.env.HARNESS_PORT || "7401", 10);
const FAILURE_COUNT = parseInt(process.env.SHADOW_FAILURE_COUNT || "1", 10);
const HARNESS_HOST = "127.0.0.1";

let remainingFailures = FAILURE_COUNT;

const proxy = http.createServer((clientReq, clientRes) => {
  // Read the full request body
  const chunks: Buffer[] = [];
  clientReq.on("data", (chunk: Buffer) => chunks.push(chunk));
  clientReq.on("end", () => {
    const body = Buffer.concat(chunks);
    const isDeploy = clientReq.method === "POST" && clientReq.url === "/v1/deployments";

    if (isDeploy && remainingFailures > 0) {
      remainingFailures--;
      console.error(`[failure-proxy] INJECTING FAILURE on deploy (remaining=${remainingFailures})`);

      // Return definitive rejection
      const respBody = JSON.stringify({
        "protocol_version": "deployment.effect.v0",
        "ok": false,
        "error_code": "service_unhealthy",
      });
      clientRes.writeHead(422, {
        "Content-Type": "application/json",
        "Content-Length": Buffer.byteLength(respBody),
        "Connection": "close",
      });
      clientRes.end(respBody);
      return;
    }

    // Forward to real harness
    const options = {
      hostname: HARNESS_HOST,
      port: HARNESS_PORT,
      path: clientReq.url,
      method: clientReq.method,
      headers: { ...clientReq.headers, "Connection": "close" },
      timeout: 30000,
    };

    const harnessReq = http.request(options, (harnessRes) => {
      clientRes.writeHead(harnessRes.statusCode || 500, harnessRes.headers);
      harnessRes.pipe(clientRes);
    });

    harnessReq.on("error", (err) => {
      console.error(`[failure-proxy] forward error: ${err.message}`);
      clientRes.writeHead(502, { "Content-Type": "application/json" });
      clientRes.end(JSON.stringify({ error: "upstream_unreachable" }));
    });

    harnessReq.on("timeout", () => {
      harnessReq.destroy();
      clientRes.writeHead(504, { "Content-Type": "application/json" });
      clientRes.end(JSON.stringify({ error: "upstream_timeout" }));
    });

    if (body.length > 0) harnessReq.write(body);
    harnessReq.end();
  });
});

proxy.listen(PROXY_PORT, HARNESS_HOST, () => {
  console.error(`[failure-proxy] listening on ${HARNESS_HOST}:${PROXY_PORT}, forwarding to ${HARNESS_HOST}:${HARNESS_PORT}`);
  console.error(`[failure-proxy] failure_count=${FAILURE_COUNT}`);
});
