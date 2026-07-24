//! HTTP client for Kernel and shadow service requests.

import * as http from "node:http";

const KERNEL_PORT = parseInt(process.env.AGENT_CORE_KERNEL_PORT || "4130", 10);
const KERNEL_BASE = `http://127.0.0.1:${KERNEL_PORT}`;

/** Make a raw HTTP request to the Kernel API. */
export function kernelRequest(
  method: string,
  path: string,
  body?: any,
  token?: string,
): Promise<any> {
  return new Promise((resolve, reject) => {
    const url = new URL(path, KERNEL_BASE);
    const opts: http.RequestOptions = {
      method,
      hostname: url.hostname,
      port: url.port,
      path: url.pathname + url.search,
      headers: { "Content-Type": "application/json" },
      timeout: 120_000,
    };
    if (token) opts.headers = { ...opts.headers, Authorization: `Bearer ${token}` };
    const req = http.request(opts, (res) => {
      let data = "";
      res.on("data", (chunk: string) => (data += chunk));
      res.on("end", () => {
        try {
          const parsed = JSON.parse(data);
          resolve({ status: res.statusCode, ok: res.statusCode! < 400, data: parsed, body: parsed });
        } catch {
          resolve({ status: res.statusCode, ok: false, data, error: "json_parse_error" });
        }
      });
    });
    req.on("error", (err) => reject(err));
    req.on("timeout", () => { req.destroy(); reject(new Error("timeout")); });
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

/** Sleep for a given number of milliseconds. */
export function sleep(ms: number): Promise<void> {
  return new Promise(r => setTimeout(r, ms));
}
