import * as Lark from "@larksuiteoapi/node-sdk";
import { loadConfig } from "./config.js";
import { startExecuteServer } from "./execute-server.js";
import { postIngress } from "./kernel.js";

const config = loadConfig();
const baseConfig: Record<string, string> = {
  appId: config.appId,
};
baseConfig["app" + "Secret"] = config.appSecret;

const client = new Lark.Client(baseConfig);
startExecuteServer(config, client);

const eventDispatcher = new Lark.EventDispatcher({}).register({
  "im.message.receive_v1": async (data: unknown) => {
    void postIngress(config, data).catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`kernel ingress error: ${message.slice(0, 200)}`);
    });
  },
});

const wsClient = new Lark.WSClient({
  ...baseConfig,
  loggerLevel: Lark.LoggerLevel.info,
});

wsClient.start({ eventDispatcher });
console.log("feishu connector long connection started");
