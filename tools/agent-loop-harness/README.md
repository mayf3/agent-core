# Agent Loop Harness V0 (Bootstrap phase)

外部 Agent Loop Harness —— 同 Session 自动续跑 V0。

## 职责（只做三件事）

1. **观察**：轮询 Kernel `GET /v1/events?cursor=N`，从通用 Journal 终态事件推导 Run outcome；
2. **策略**：在 Harness 内应用 `run.outcome.resolve.v0` V0 默认策略（yielded 且未超限 → continue_same_session）；
3. **续跑**：需要继续时 `POST /v1/session-continuation`（仅携带 `trigger_run_id` + 确定性
   `idempotency_key=continuation:<trigger_run_id>`）请求 Kernel 在**同一 Session** 创建下一 Run。

## 边界

- 只通过 Kernel 公开 HTTP 窄契约交互；**不依赖 agent-core-kernel crate、不读 Kernel DB、不理解产品语义**；
- Kernel 不调用 `run.outcome.resolve.v0`；策略逻辑在本 Harness 内；
- **Harness 不是身份/路由/会话事实的来源**：Kernel 根据 `trigger_run_id` 从自身记录恢复
  session、principal、channel、conversation target、Registry Snapshot；
- 不建设 task / progress / checkpoint / Development Job / Orchestrator；
- 本地状态（cursor、processed run ids、自动续跑计数、总墙钟时间、连续失败数）只用于**防重复与防无限循环**，
  不是任务管理系统。即使 state.json 丢失，Kernel 的 `UNIQUE(trigger_run_id)` 也保证同一 trigger 只续跑一次。

## 配置（环境变量）

| 变量 | 默认 | 说明 |
|---|---|---|
| `AGENT_LOOP_KERNEL_URL` / `KERNEL_API_URL` | `http://127.0.0.1:4130` | Kernel HTTP 地址 |
| `AGENT_LOOP_IPC_TOKEN` / `AGENT_CORE_IPC_TOKEN` | —（必填） | Kernel IPC token |
| `AGENT_LOOP_STATE_PATH` | `<data_dir>/agent-loop-harness/state.json` | 本地状态文件（重启恢复用） |
| `AGENT_LOOP_MAX_AUTOMATIC_RUNS` | `5` | 自用户输入以来最大自动续跑次数 |
| `AGENT_LOOP_MAX_TOTAL_WALL_TIME_MS` | `600000` | 自用户输入以来最大续跑墙钟总时长 |
| `AGENT_LOOP_MAX_CONSECUTIVE_FAILURES` | `3` | 最大连续失败次数 |
| `AGENT_LOOP_POLL_INTERVAL_MS` | `500` | 事件轮询间隔 |

## 运行

```bash
AGENT_LOOP_KERNEL_URL=http://127.0.0.1:4130 \
AGENT_LOOP_IPC_TOKEN=<token> \
cargo run -p agent-loop-harness
```

## 架构文档

见 `docs/architecture/AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1.md`（§7 Bootstrap、§5 Run Outcome Hook）。
