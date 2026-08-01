# OpenClaw 平替差距调查与分阶段迁移方案 V0（修订版）

> **状态**: 纯调查文档（research-only），不包含实现。
> **性质声明**: 本文档是**跨仓库调查索引**，不代表相关实现归 Agent Core 维护。
> **调查日期**: 2026-08-01（本修订版同日，补充上游基线、真实流量证据与边界修正）
> **调查范围**: OpenClaw 双版本基线（用户已装 2026.3.13 + 上游最新正式 release 2026.7.1）+ 用户本机实例脱敏画像 + Agent Core 生态现状 + 能力矩阵 + 分阶段替代路线。
> **本轮不做**: 不开发 Router、不开发新 Harness、不修改 Kernel、不迁移仓库、不创建仓库、不启动/停止服务、不改代码/配置/数据库。

---

## 0. Executive Summary

### 0.1 双版本基线

```text
USER_INSTALLED_BASELINE=2026.3.13     （本机 npm 全局安装，Gateway 正在运行）
CURRENT_UPSTREAM_BASELINE=2026.7.1   （GitHub release tag v2026.7.1，
                                        source SHA 2d2ddc43d0dcf71f31283d780f9fe9ff4cc04fe4，
                                       published 2026-07-13；npm dist-tag latest=2026.7.1-2）
```

- 用户当前实际拥有：**2026.3.13**（比上游最新落后约 4 个月 / 若干 minor 版本）。
- 上游 2026.4→2026.7 新增内容中，与本调查直接相关的最大项是**官方 Dreaming**（memory-core 扩展，见 §1.10）。
- 需要区分三类能力：
  - **用户自建**（上游也没有，或用户未用上游实现）：brain-memory-system 睡眠整合、token/session 监控脚本、auto-repair daemon；
  - **上游已有、可直接复用**：官方 Dreaming（opt-in）、memory-search 向量检索、QMD backend、LanceDB 长记忆、Context Engine 插件、ACP bridge、sub-agents；
  - **仍需自行建设**（用户侧与 Agent Core 侧都缺）：无（本项在 Agent Core 语境下指 Kernel/外部组件契约，见 §4）。

### 0.2 事实摘要

用户当前的 OpenClaw 实例是一个**重度使用、多 Agent、多频道、高度自定义**的个人 Agent 基础设施：

- 79 个 Feishu 群绑定 Agent，每 Agent 独立 workspace + SOUL.md/AGENTS.md；
- 177 个 cron 任务（138 个启用），覆盖每日学习、周内化、应用检查、夜间自动审稿等；
- 每日 session reset（默认 daily 4AM）+ 预压缩 memory flush（即用户口中的"每晚清空 session"，细节见 §1.6）；
- 自建 `brain-memory-system` skill 提供"睡眠整合"式长期记忆（即用户口中的"夜间做梦"，细节见 §1.10）；
- 外部生态已分家：svc-workflow / svc-okr / auth-service / workflow-todo / agent-forum / llm-wiki 等独立仓库在跑。

Agent Core 生态现状：

- Kernel 边界执行非常严格（负面宪法 + 外部编排边界 + primitive calculus），**未发现 WRONG_BOUNDARY 严重违规**；
- Kernel + Feishu Connector 在 Lima VM（agent-core-hcr）中运行，**Feishu→Kernel 真实链路已经存在并承载实际消息**（107/108 runs 来自 Feishu，含 run_bd2fc177c7504c7ca0848df77d6d17c9、run_27d0098144504011a678c446521b6251，详见 §3.4）；
- 但当前流量尚未承载**可替代 OpenClaw 的稳定主 Agent 体验**（无 AGENTS.md/SOUL.md 注入、无记忆、无跨日 session 连续性）；
- Coding/Deployment/Capability/Context-Hook Harness 都在 `tools/` 内但逻辑上属于外部组件；
- Agent Forum / Workflow / Auth 已迁出为独立 GitHub 仓库；
- **上下文真实组装、记忆、压缩、Router、cron/心跳、后台任务、多渠道呈现全部 MISSING**——恰好与 OpenClaw 用户最依赖的能力重合。

### 0.3 结论

真实差距不是"Kernel 不够强"，而是**外部产品层（Context 组装、记忆、Session 生命周期、调度、路由、呈现）一片空白**。迁移的第一阶段不应是 Router，而应是**一条可从 OpenClaw 迁走的真实纵向使用链路**：把用户的一个真实 Agent（固定身份 + 上下文文件 + 记忆 + 跨日消息 + 压缩 + memory flush + Feishu 真实对话 + 外部工具）完整迁移到 Agent Core + 外部 Runtime 上连续可用，并实际减少 OpenClaw 流量。Router、复杂 Dreaming、全部 cron、sub-agent 和 ACP 不进入 P1。

---

## 1. OpenClaw 真实能力模型（官方事实）

> 事实来源: 本机安装的 2026.3.13 官方 `docs/` 与 `dist/`；上游最新 2026.7.1（source SHA `2d2ddc43d0dcf71f31283d780f9fe9ff4cc04fe4`）的官方 `docs/` 与 `extensions/`。两版本冲突处明确标注。

### 1.1 版本与来源

- 用户安装: `openclaw@2026.3.13`（npm 全局，`/usr/local/lib/node_modules/openclaw`）
- 上游最新正式: `openclaw@2026.7.1`（`https://github.com/openclaw/openclaw`，tag `v2026.7.1`，SHA `2d2ddc43d0dcf71f31283d780f9fe9ff4cc04fe4`；npm `latest` dist-tag 为 `2026.7.1-2`，`extended-stable` 为 `2026.6.33`）
- 架构: 单 Gateway 进程 + WS 协议客户端（macOS app / CLI / Web UI / Nodes）

### 1.2 Agent runtime 与主 Agent Loop

- Gateway RPC `agent` / `agent.wait` 是入口（`docs/concepts/agent-loop.md`）。
- 内部运行 `runEmbeddedPiAgent`（pi-agent-core runtime）：per-session + global 队列串行化 → 构建 pi session → 流式事件（assistant / tool / lifecycle）。
- `agent.wait` 等待 lifecycle end/error，返回 `{status, startedAt, endedAt}`。
- **对应 Agent Core**: Kernel `Run` 生命周期 + `Runtime`（`src/runtime/`）已是等价物，但无"主 Agent 循环"概念——Kernel 刻意不为模型跑 loop，loop 属于外部 Harness。

### 1.3 Gateway 与 channel/plugin 边界

- 单 Gateway 持有所有 messaging surfaces，控制面客户端走 WS（默认 `127.0.0.1:18789`）。
- 插件是 TypeScript 模块（jiti 加载），可注册 tools / network handlers / hooks / commands / services；官方插件含 Memory (Core)、Memory (LanceDB)、Voice Call、Matrix、Teams 等。
- **对应 Agent Core**: Feishu Connector（`connectors/feishu`）是唯一 channel adapter；Kernel 与 Connector 通过 IPC（`/v1/ingress` + `/v1/execute`）解耦——**架构对应，但渠道数量差距大**（用户只实际使用 Feishu，所以此差距不影响迁移）。

### 1.4 Agent workspace / agentDir / session store

- workspace: 默认 `~/.openclaw/workspace`（每 Agent 独立，多 Agent 时 `agents.list[].workspace`）
- agentDir: `~/.openclaw/agents/<agentId>/agent`（auth profiles、model registry 独立）
- session store: `~/.openclaw/agents/<agentId>/sessions/sessions.json` + `<SessionId>.jsonl`
- **对应 Agent Core**: 已有 `docs/architecture/external-harness-workspace-v0.md` 规划 `~/.agent-core/harnesses/<name>/`；VM 运行时已有 `~/.agent-core/agents/main` 与 `context-provider` 独立进程——但**无 per-agent workspace 规范落地**。

### 1.5 上下文文件加载规则（AGENTS.md / SOUL.md / 等）

官方注入集合（若存在即注入，`docs/concepts/context.md`、`docs/concepts/agent-workspace.md`；2026.3.13 与 2026.7.1 一致）：

| 文件 | 加载时机 | 用途 |
|---|---|---|
| `AGENTS.md` | 每 session 开始 | 操作指令、记忆用法 |
| `SOUL.md` | 每 session | 人设、语气、边界 |
| `USER.md` | 每 session | 用户画像 |
| `IDENTITY.md` | bootstrap 仪式创建 | 名字、风格、emoji |
| `TOOLS.md` | 每 session | 工具说明（不控制可用性） |
| `HEARTBEAT.md` | heartbeat run | 心跳检查清单 |
| `BOOT.md` | gateway 重启（internal hooks 开启时） | 启动清单 |
| `BOOTSTRAP.md` | 首次运行 | 一次性出生仪式，完成后删除 |
| `memory/YYYY-MM-DD.md` | session 开始读今天+昨天 | 日记忆 |
| `MEMORY.md` | 仅主会话 | 长期记忆 |

- 截断: `bootstrapMaxChars` 默认 20000 字符/文件，`bootstrapTotalMaxChars` 默认 150000 总量。
- 用户实际主 workspace: AGENTS.md 415 行、SOUL.md 36 行、USER.md 17 行、IDENTITY.md 23 行、TOOLS.md 40 行、HEARTBEAT.md 5 行（**MEMORY.md 缺失**，说明该用户长期记忆实际走 memory/ + 外部 llm-wiki）。
- **对应 Agent Core**: `ContextBlock`（`src/domain/context_block.rs`）已有 `AgentProfile`/`SkillCatalog`/`ToolCatalog`/`RecentMessages` 等 kind；`context.prepare.v0` hook 契约在 Kernel 中**完整存在且已接入运行**（见 §1.7），但当前 Provider 是 passthrough 模式，**没有实际的外部上下文文件读取与注入逻辑**。

### 1.6 Session key / daily reset / idle reset / `/new` / `/reset`

- session key: `agent:<agentId>:<mainKey>`（DM 折叠到 main）；group 独立；cron 用 `cron:<job.id>`。
- **版本默认（2026.3.13 与 2026.7.1 一致）**: daily reset 默认启用，**默认 4:00 AM 网关本机时间**。注意区分：
  - `session.reset` 未配置时的行为 = **daily 模式默认启用，atHour=4 是模式启用后的默认值**（不是"需要显式开启 daily reset"）；
  - 也可 `idleMinutes` 滑窗（旧版 `session.idleMinutes` 单独设置时回退 idle-only 兼容模式）；
  - 两者先到先重置。
- **reset 语义**: 触发时**新建 sessionId**（会话连续性从新 session 开始），**不删除历史**；旧 transcript `<SessionId>.jsonl` 与 `sessions.json` 条目保留（`session.maintenance.pruneAfter` 默认 30d 后才可能被清理），旧 session 内容可通过旧 sessionId 追溯。
- `/new` `/reset` 手动重置；isolated cron 每 run 新 session。
- **用户显式配置**: `session.dmScope=per-channel-peer`；`session.reset` / `resetByType` / `idleMinutes` **均未显式配置** → 实际行为 = 版本默认 daily 4AM + 主会话 memory flush 定制 prompt（§1.7）。
- **自定义 cron 行为**: 用户另有自建 session 监控/清理脚本（`scripts/check-session-size.sh`、`emergency-compact.sh`、`auto-cleanup.sh` 等），按大小阈值人工/定时干预，与官方 daily reset 并行。
- **对应 Agent Core**: Kernel 有 Session 身份/状态事实（`SessionTarget` = agent_id + channel + conversation_key；`get_or_create_session` 创建/复用；`SessionStatus`；`summarized_until_event_id` + `summary` 压缩边界字段），**Session 创建/复用/切换的通用治理原语存在**（§4 核对）。**daily/idle reset 策略、重置时间、重置后的 Context 装载策略属于外部 Agent Runtime，Kernel 无此产品语义（且不应有）**。

### 1.7 Context assembly / pruning / compaction / memory flush

- System prompt = base + skills 列表 + bootstrap context + per-run overrides；`/context list` 可查注入清单。
- Pruning: `contextPruning.mode=cache-ttl`（用户: 30m）——旧 tool result 内存修剪。
- Compaction: 摘要旧历史持久化进 JSONL；`identifierPolicy=strict`（用户配置）保留不透明标识符；可用独立 compaction model。
- **Memory flush**: 接近自动压缩阈值时触发一次**静默 agentic turn**，提醒模型把持久记忆写入 `memory/YYYY-MM-DD.md`，回复 `NO_REPLY` 则不打扰用户（用户配置 enabled，softThresholdTokens=15000，中文 prompt 定制）。
- 用户实例 compaction: mode=default, reserveTokens=16384, keepRecentTokens=30000, reserveTokensFloor=80000。
- **对应 Agent Core（本次修订重点核对）**: `context.prepare.v0` 是**已存在的通用契约**而非缺失原语——Kernel 侧 Hook ABI 完整（`src/config.rs` 配置项、`src/hook/context_artifact.rs` 的 ImmutableArtifactRef/OpaqueArtifactRef/HMAC 证明、`src/server/delivery.rs` 的 hook 装配、`src/server/hook_wiring_tests.rs` 生产路径测试）；VM 运行时 `AGENT_CORE_CONTEXT_PREPARE_HOOK_ENABLED=true` 且 context-provider（`~/.agent-core/runtime/context-provider/server.ts`，:17400）正在运行。**缺的不是契约，而是 Provider 侧的真实上下文组装实现**（当前 server.ts 是 passthrough 模式，仅签名证明）。**不要在 Kernel 里重复建设 context 契约**；建设点是外部 Provider 服务（§5）。

### 1.8 Context Engine 插件机制

- `plugins.slots.contextEngine` 选择 context engine 插件（默认 `legacy` 内建；可换 `lossless-claw` 等）；`kind: "context-engine"` 的插件拥有 assemble/ingest/ownsCompaction。
- 用户实例: **无 contextEngine 插件启用**（openviking 扩展存在但 disabled）。
- **对应 Agent Core**: Context Provider 是已规划的外部组件（`TargetKind::ContextProvider` in `src/domain/self_evolution.rs`），契约 `context.prepare.v0` **已存在且已接线**（见 §1.7），Provider 真实实现待建设。

### 1.9 普通记忆 / daily notes / Active Memory / 搜索召回

- Markdown 文件即事实源；`memory_search`（语义召回）+ `memory_get`（定点读取）两个工具。
- 向量索引: 默认启用，`memorySearch.provider` 自动选择；`sqlite-vec` 加速。
- QMD backend（实验）: BM25+向量+rerank 本地 sidecar（用户**未启用**）。
- Memory (LanceDB) 插件: auto-recall/capture 长期记忆（用户**未启用**）。
- 用户实际: 默认 memory-core（`plugins.slots.memory` 为空），`~/.openclaw/memory` 5.3M 日记忆，groups workspace 内另有 MEMORY.md（79 个 SOUL.md/AGENTS.md 全部存在）。
- **对应 Agent Core**: 记忆完全属于外部（`MemoryStrategy` 明列在 external-orchestration-boundary 禁止入 Kernel 清单）。**现状: 无记忆外部组件。**

### 1.10 Dreaming（官方 2026.7.1 有；用户安装的 2026.3.13 无）——重点修正

**版本事实（本次修订核心修正）:**

| 维度 | 事实 | 证据 |
|---|---|---|
| 用户安装版本 | **2026.3.13 中未发现官方 Dreaming**（dist 中无 dreaming 模块；memory-core 扩展仅 index.ts/plugin.json/package.json） | `find /usr/local/lib/node_modules/openclaw -iname "*dream*"` 无结果 |
| 当前上游 | **2026.7.1 已有官方 Dreaming**：memory-core 扩展内 `dreaming*.ts`（phases/narrative/markdown/shadow-trial/repair/events/state）+ 官方文档 `docs/concepts/dreaming.md` + Dreams UI + CLI（`/dreaming status|on|off`、`openclaw memory promote`） | upstream tree `extensions/memory-core/src/dreaming-*.ts`；`docs/concepts/dreaming.md` |
| 默认启用? | **opt-in，默认禁用**（"Dreaming is opt-in and disabled by default"） | `docs/concepts/dreaming.md` 顶部 Note |
| 阶段模型 | **light → REM → deep 三阶段**（内部实现阶段，非用户配置模式）：Light 排序暂存短期素材（不写 MEMORY.md）；REM 主题反思（不写）；Deep 打分并晋升到 MEMORY.md（阈值 minScore/minRecallCount/minUniqueQueries 全过） | `docs/concepts/dreaming.md` Phase model 表 |
| 产出 | `memory/.dreams/`（机器状态）、`DREAMS.md`（人类可读 Dream Diary + Deep Sleep 摘要）、可选 `memory/dreaming/<phase>/YYYY-MM-DD.md`；长期晋升只写 MEMORY.md | 同文档 "What dreaming writes" |
| 调度 | `dreaming.frequency` 默认 `0 3 * * *`（cron）；`dreaming.model` 可指定日记子 Agent 模型 | 同文档 Scheduling |
| 去重/防循环 | Deep 排名 6 信号加权（relevance .30 / frequency .24 / query diversity .15 / recency .15 / consolidation .10 / conceptual richness .06）+ phase 强化 + shadow-trial（report-only） | 同文档 Deep ranking signals |

**文档冲突记录**: 上游 2026.7.1 官方文档内部未发现相互矛盾（opt-in 禁用、light/REM/deep 为内部阶段而非用户模式、DREAMS.md 不参与晋升）。唯一需要留意的语义差异：早期 beta（2026.7.1-beta.x）中 dreaming 曾以 cron 插件形态存在，正式版收敛为 memory-core 内置 + 自动管理 cron（同一文档 Scheduling 节说明 "auto-manages one cron job"）。**若后续发现社区文档与官方不一致，以官方 docs/concepts/dreaming.md + 固定 SHA 源码为准。**

**用户实际"夜间做梦"来源（用户自建）:** `brain-memory-system` skill（`cognitive-brain`）：episodic(海马)/semantic(新皮层)/procedural(小脑)/attention(丘脑)/sleep replay 整合/soul erosion 健康指标，SQLite + LLM 驱动，`brain consolidate` = sleep replay (hippocampal consolidation)。该 skill 挂在用户 workspace，**未挂任何 cron**（近期脑整合实际未定时运行）。

> 结论: 用户"夜间做梦"= 自建 skill 的睡眠整合；上游 2026.7.1 已有官方 Dreaming（opt-in）。迁移评估时应分别考虑：官方 Dreaming 可作为"可直接复用上游"的候选；用户自建 brain 系统保持独立外部组件地位；两者都不是 Kernel 能力。

### 1.11 记忆来源 / 可信度 / 去重 / 防召回循环 / 晋升

- 官方 2026.3.13: 无系统级记忆来源可信度分级；靠"模型自己写对"。官方 2026.7.1 Dreaming 增加了打分晋升（§1.10），但记忆写入仍以文件为源。
- 用户侧: `brain-memory-system` 提供去重（FTS5）与冲突检测（soul erosion），但无"防召回循环"显式机制。
- **对应 Agent Core**: 全部缺失（外部）。

### 1.12 Multi-agent bindings / 隔离 workspace / 独立 session

- `agents.list[]` + `bindings[]`（channel + peer match → agentId）；每 Agent 独立 workspace/agentDir/session store/auth profiles。
- 用户实例: **79 个 Agent**，全部绑定 Feishu 群（`peer.kind=group`），每个有独立 `groups/workspace-oc_*` 目录，79 个 SOUL.md + 79 个 AGENTS.md + 72 个 skills 目录。
- **对应 Agent Core**: 多 Agent = 外部编排 + 多个普通 Run + 不同 Workspace/Grants（`external-orchestration-boundary.md` §关于多 Agent）。**现状: 无外部编排、无 Router。**

### 1.13 sessions_spawn / background tasks / sub-agent 回传 / 嵌套

- `sessions_spawn` tool: 后台 sub-agent 独立 session（`agent:<id>:subagent:<uuid>`），完成后 announce 回 requester chat；支持嵌套、`--model` 覆盖、thread 绑定、`mode:run|session`。
- 用户实例: `~/.openclaw/subagents/runs.json` 存在但 **0 个 run**（当前不太用 sub-agent 工具链；"多 Agent"是绑定路由而非 spawn）。
- **对应 Agent Core**: 无（外部）。Kernel 的 outbox/worker 是投递可靠性原语，不是 sub-agent 语义。

### 1.14 ACP 外部 Harness 接入

- `openclaw acp` 是 Gateway-backed ACP bridge（stdio ↔ Gateway WS）；`sessions_spawn` 支持 `runtime:"acp"`（Codex/Claude Code/Gemini CLI）。
- 用户实例: `~/.openclaw/acpx/codex-acp-wrapper.mjs` 存在（Codex ACP wrapper）。
- **对应 Agent Core**: 无 ACP；Coding Harness 是自有协议（`external.coding_*` 操作），与 ACP 正交。

### 1.15 cron / heartbeat / hooks / standing orders / 主动执行

- **Cron**: 精确调度、isolated session、`delivery.mode=announce` 回频道；top-of-hour 自动加 0-5min 抖动。
- **Heartbeat**: 主 session 周期 agent turn（默认 30m），读 HEARTBEAT.md，`HEARTBEAT_OK` ack 抑制空消息，可 activeHours。
- **Hooks**: 内部 hooks（`agent:bootstrap`、command hooks `/new` `/reset` `/stop`）+ 插件 hooks（`before_prompt_build`、`before/after_compaction`、`before/after_tool_call`、`message_received/sending/sent`、`session_start/end`、`gateway_start/stop`）。官方内置 4 个 hook（session-memory、bootstrap-extra-files、command-logger、boot-md）。
- 用户实例: 177 个 cron jobs（138 enabled），全部 `sessionTarget=isolated` + Feishu announce；cron 集中在**每日学习（29）、周内化（21）、应用检查（20）、夜间审稿（7）、随想（4）**等模式；另 crontab 有健康监控、备份任务；launchd 有 auto-repair daemon + gateway plist。
- **对应 Agent Core**: cron/heartbeat 属于外部（`SchedulerBusinessRule` 禁止入 Kernel）；Kernel 有 `event.observe.v0`（Journal 事实可被外部消费）与 Run Budget Hook（`src/hook/budget.rs`）——**订阅/触发基础在，调度产品层无**。

### 1.16 用户界面：进度 / 审批 / 主动通知 / 失败呈现

- Web Control UI、macOS app、CLI、WebChat、canvas host（`/__openclaw__/canvas/` + `/a2ui/`）、Nodes。
- 用户实例: 主要呈现面是 **Feishu 群消息**；辅以 token-monitor 脚本 + Feishu webhook 告警。
- **对应 Agent Core**: Kernel 有 durable Approval（`m2d-durable-approval`，`AwaitingApproval` run 状态 + `/v1/approve`）——**审批比 OpenClaw 强**；但无进度流/失败 UI（外部）。

---

## 2. 用户实际使用画像（脱敏证据索引）

> 以下全部来自只读调查；未输出任何 token/密钥/消息正文/完整记忆。

### 2.1 实例事实表

| 项 | 值 | 证据 |
|---|---|---|
| OpenClaw 版本 | 2026.3.13 | `/usr/local/lib/node_modules/openclaw/package.json` |
| Gateway | 运行中，PID 60755，:18789 | `ps` + `lsof` |
| Agent 数 | **79** | `openclaw.json agents.list` |
| 频道 | 仅 Feishu（websocket 连接） | `channels.feishu` |
| Bindings | 79 个全部 = feishu group → agent | `bindings[]` |
| 主模型 | zai/glm-5.2（fallback: opencode-go/deepseek-v4-flash, deepseek/deepseek-v4-flash） | `agents.defaults.model` |
| workspace | 主 `~/.openclaw/workspace` + 79 个 `groups/workspace-oc_*` | 文件系统 |
| 上下文文件 | 主 ws: AGENTS/SOUL/USER/IDENTITY/TOOLS/HEARTBEAT 全存在；**MEMORY.md 缺失** | `ls` + `wc -l` |
| Session reset | **未显式配置** → 版本默认 daily 4AM（见 §1.6） | `openclaw.json` 无 `session.reset` |
| dmScope | per-channel-peer（显式） | `session.dmScope` |
| Compaction | mode=default; reserve 16384; keepRecent 30000; floor 80000; strict identifiers; **memoryFlush enabled**（中文定制 prompt） | `agents.defaults.compaction` |
| Context pruning | cache-ttl 30m | `agents.defaults.contextPruning` |
| 记忆 provider | 默认 memory-core（无 qmd、无 lancedb、无 memorySearch 配置） | `plugins.slots` 为空 + 无 `memory.*` |
| Cron | **177 jobs / 138 enabled**，全部 isolated + announce | `~/.openclaw/cron/jobs.json` |
| 插件 allow | acpx, openclaw-lark, feishu, memory-core, openclaw-auth-broker | `plugins.allow` |
| 插件 entries | acpx, openclaw-lark, feishu, openviking(disabled), openclaw-auth-broker | `plugins.entries` |
| Skills | workspace: agent-forum, brain-memory-system, fun-denoise, speech-denoise；shared: agent-lifecycle, brave-browser-agent, cron-helper, daily-learning, workflow-system；另有 ~/.agents/skills 13 个 | 文件系统 |
| Sub-agent | runs.json 存在但 **0 run** | `~/.openclaw/subagents/runs.json` |
| ACP | codex-acp-wrapper.mjs 存在 | `~/.openclaw/acpx/` |
| 自建脚本 | session-size monitor、emergency-compact、compaction monitor、token monitor、auto-cleanup、daily-check、auto-repair daemon | `~/.openclaw/scripts/` |
| 记忆体量 | `~/.openclaw/memory` 5.3M；agents 目录 2.1G（session JSONL 3138 个） | `du` / `find` |
| 外部协作服务 | auth-service:4001, svc-workflow:8989, svc-forum:3460, article-review:3002/17231, llm-wiki 等 | `lsof` + 项目目录 |

### 2.2 用户声称的关键依赖 → 实际配置/执行证据

| 用户说法 | 证据 | 判断 |
|---|---|---|
| 夜间做梦 | 安装版 2026.3.13 无官方 Dreaming（上游 2026.7.1 有，opt-in）；用户实际来自自建 `brain-memory-system`（sleep replay/hippocampal consolidation），未挂 cron；夜间任务实际是"夜间自动审稿 8 轮"等 cron | 自建 skill + 夜间 cron 混合；上游可复用官方 Dreaming |
| 自动反思 | delivery-review-agent（复盘专家）、soul-questioner（每日灵魂拷问）、各 Agent"周内化/应用检查" cron | 通过 cron + 专用 Agent 实现 |
| 动态压缩 | compaction.mode=default + memoryFlush + contextPruning cache-ttl 30m | 官方机制启用 |
| 每晚清空 session | 未显式配置 → 版本默认 daily reset atHour=4；memoryFlush prompt "会话即将重置清理"；reset = 新建 sessionId，旧 transcript 保留可查（30d 维护期） | 官方默认 + 定制 prompt |
| 多 Agent Router | bindings 79 条 feishu group→agent 路由 | 官方 bindings 机制 |
| 跨任务和长期记忆 | memory/YYYY-MM-DD.md + llm-wiki synthesis + knowledge-base + brain-memory-system + 各 group MEMORY.md | 混合（官方文件记忆 + 外部 llm-wiki） |

### 2.3 用户最近真实使用中最依赖的能力（脱敏排序）

1. **多 Agent 群路由**（79 个 Feishu 群，每个群一个专家 Agent）——这是日常入口；
2. **Cron 主动任务**（138 个启用任务：每日学习/周内化/应用检查/审稿/简报）——占运行量主体；
3. **Session 每日重置 + 预压缩记忆落盘**（"每晚清空 + 记得写 memory"）——跨日连续性的核心；
4. **上下文文件注入**（每个 Agent 的 SOUL/AGENTS 决定人设与边界）；
5. **长期记忆与知识沉淀**（memory/ + llm-wiki + brain-memory-system）；
6. 审批/失败呈现：低（Feishu 群 + 自建 token 监控脚本足够）。

---

## 3. Agent Core 生态仓库盘点

### 3.1 分类方法

以 git remote / README / manifest / 运行路径 / 数据库证据为判据，不按目录名猜。

### 3.2 盘点表

| 组件 | 类别 | 证据 | 位置 | 成熟度 |
|---|---|---|---|---|
| Kernel（Rust） | IN_KERNEL | `Cargo.toml` package=agent-core-kernel；`src/` 169 个 rs | agent-core 主仓 | 生产可用（M0-M5+ 全 Done；VM 运行中 :4130） |
| Feishu Connector (TS) | IN_REPO_EXTERNAL | `connectors/feishu/`，README 自述"不是 Kernel" | agent-core 主仓 | 生产可用（VM 运行中 :4131；真实 Feishu 流量） |
| Coding Harness | IN_REPO_EXTERNAL | `tools/coding-harness/`，自持 Cargo.toml package=coding-harness，已 build | agent-core 主仓 | 生产可用（external.coding_* 已注册 + HCR 实测） |
| Capability Host | IN_REPO_EXTERNAL | `tools/capability-host/` package=capability-host，已 build | agent-core 主仓 | 可用（invocable capability 生成链） |
| Deployment Harness | IN_REPO_EXTERNAL | `tools/deployment-harness/` package=deployment-harness，已 build，`deployment.effect.v0` | agent-core 主仓 | 可用（升级/回滚/禁用） |
| Context Hook Harness / Context Provider | IN_REPO_EXTERNAL + RUNNING | `tools/context-hook-harness/`（契约）；VM `~/.agent-core/runtime/context-provider/server.ts`（passthrough 模式运行中 :17400，HMAC 证明完整） | agent-core 主仓 + VM 运行时 | 接线完成，Provider 真实组装逻辑待建设 |
| Shadow Failure Proxy / Shadow Canary | IN_REPO_EXTERNAL | `tools/shadow-failure-proxy/` + `tools/shadow-canary/` | agent-core 主仓 | 实验（脏升级影子） |
| Replay/Eval | IN_REPO_EXTERNAL | `tools/replay-eval/` | agent-core 主仓 | MVP（evolution 门禁） |
| Evolution Harness | IN_REPO_EXTERNAL | `tools/evolution-harness/` | agent-core 主仓 | plan+report only |
| Audit Report | IN_REPO_EXTERNAL | `tools/audit-report/` | agent-core 主仓 | 只读审计 CLI |
| Agent Forum (svc-forum) | SEPARATE_REPO | `github.com/mayf3/agent-forum`；运行 :3460 | 独立仓库 | 运行中（build readiness 分支） |
| Auth Service | SEPARATE_REPO | `github.com/mayf3/auth-service`；运行 :4001 | 独立仓库 | 运行中（OAuth2 client_credentials + OBO） |
| svc-workflow | SEPARATE_REPO | `root@8.163.44.127:/opt/git/svc-workflow.git`（canary remote）；运行 :8989 | 独立仓库/服务器 | 运行中 |
| svc-okr | SEPARATE_REPO | `github.com/mayf3/svc-okr` | 独立仓库 | 运行中 |
| workflow-todo | SEPARATE_REPO | `github.com/mayf3/workflow-todo` | 独立仓库 | 运行中 |
| llm-wiki | SEPARATE_REPO | `github.com/mayf3/llm-wiki` | 独立仓库 | 运行中（编译/synthesis 管线） |
| article-review-platform | SEPARATE_REPO | `github.com/mayf3/article-review-platform` | 独立仓库 | 运行中 |
| it-ops-control-plane | SEPARATE_REPO | `github.com/mayf3/it-ops-control-plane` | 独立仓库 | 运行中 |
| adc-v2 | RUNNING_UNVERSIONED | `~/workspace/project/adc-v2`，无 remote | 本地 | 运行但未版本化 |
| architecture-portal | RUNNING_UNVERSIONED | `~/workspace/project/architecture-portal`，非 git | 本地 | 静态站点，未版本化 |
| agent-kanban | SEPARATE_REPO | `github.com/mayf3/agent-kanban`（即 `~/.openclaw` 内容仓库） | 独立仓库 | OpenClaw 内容备份仓 |

### 3.3 结论

- **已在 Kernel 内**: Kernel 本体（职责收敛良好）。
- **在 agent-core 仓库但逻辑属于外部**: 全部 9 个 tools/ + connectors/feishu。
- **已迁出独立 GitHub 仓库**: agent-forum、auth-service、svc-okr、workflow-todo、llm-wiki、article-review-platform、it-ops-control-plane、svc-workflow(canary)。
- **已运行但未版本化**: adc-v2、architecture-portal。
- **尚不存在**: 真实 Context 组装 Provider（契约已存在，Provider 为 passthrough）、记忆组件、Compaction/Pruning 服务、Router/Multi-Run Orchestrator、Cron/Heartbeat 调度服务、Session 生命周期策略、多渠道 UI、ACP bridge、Dreaming 接入（官方有、Agent Core 无）。
- **历史实现/已退役**: builtin `time.now` 已由 PR #165 退役，改走外部 harness——符合方向。

### 3.4 Feishu→Kernel 真实流量证据（本次修订新增）

来自 VM 内运行中 Kernel 数据库 `~/.agent-core/data/agent-core.db`（Lima agent-core-hcr）：

| 指标 | 值 |
|---|---|
| 总 runs | 108（2026-07-22 ~ 2026-07-31） |
| 其中 Feishu 来源 | **107**（`principal_json` 含 `"source":"Feishu"`，principal=`feishu:open_id:ou_1e44...`） |
| 用户点名的 run | `run_bd2fc177c7504c7ca0848df77d6d17c9`（Completed，main，2026-07-31T13:59Z）、`run_27d0098144504011a678c446521b6251`（Completed，main，2026-07-31T23:37Z） |
| 状态分布 | Completed 72 / Failed 12 / Running 9 / WaitingDispatch 15 |
| Journal 事实 | IngressAccepted 96、RunStarted 96、InvocationProposed 902、InvocationApproved 855、LlmCompleted 880、HookCallRecorded 226 |
| Sessions | 15（main + 14 个 harness agent session） |
| Feishu 执行侧 | `feishu-executes.jsonl`（north-star-live 10 行 + canary 1 行）、`feishu-reactions.jsonl` 26 行 |

> 结论: **Feishu→Kernel 真实链路已经存在并承载实际消息**。但该流量是 HCR/canary 验证流量（大量 agent_* 一次性 session、Failed 12 条、WaitingDispatch 15 条），**尚未承载可替代 OpenClaw 的稳定主 Agent 体验**（无上下文文件注入、无记忆、无跨日 session 连续性策略）。

---

## 4. 完整差距矩阵

差距等级: `READY` / `PARTIAL` / `MISSING` / `WRONG_BOUNDARY` / `UNKNOWN`

| # | 能力 | OpenClaw 当前行为 | 用户真实使用 | Agent Core 生态现状 | 差距 | Kernel 或外部 | 建议所属仓库 | 依赖 | 最小验收场景 |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 多渠道入口 | 7+ 频道 + 插件 | 仅 Feishu | Feishu Connector 生产可用（真实流量） | PARTIAL | 外部 | agent-core（短期）→ 独立 connector 仓库 | Kernel IPC | Feishu 消息闭环（已有） |
| 2 | 多 Agent 群路由 | bindings 79 条 | **重度**（日常入口） | 无 Router/外部编排 | MISSING | 外部 | router harness 仓（候选，见 §6） | Context 加载 + Session 事实 | 两个群路由到两个 Agent 各回各话 |
| 3 | 上下文文件加载 | AGENTS/SOUL/USER/TOOLS/HEARTBEAT 注入 + 截断 | **重度** | `context.prepare.v0` 契约+接线+运行（passthrough Provider）；**真实组装逻辑缺** | PARTIAL | 外部 | context-provider（候选，见 §6） | Kernel hook ABI（已有） | AGENTS.md 内容出现在 system prompt（/context 可查） |
| 4 | Session key / 每日重置 | daily 4AM 默认 + /new /reset | **重度**（每晚清空） | **Kernel 通用 Session 原语齐备**（SessionTarget/get_or_create_session/Status/summary）；**外部 Session Policy 缺**（daily/idle reset、重置时间、重置后 Context 装载） | PARTIAL | Kernel 原语 + 外部策略 | 外部 agent runtime（候选，见 §6） | Kernel Session API | 跨日第二条消息自动进新 session（旧 transcript 可查） |
| 5 | 动态压缩 | auto-compaction + 独立模型 + identifierPolicy | **重度** | 无 | MISSING | 外部 | agent runtime（候选，见 §6） | Context 组装 + 模型 | 超窗消息被摘要压缩且可追溯 |
| 6 | 预压缩 memory flush | 静默 agentic turn | **重度**（中文定制） | 无 | MISSING | 外部 | agent runtime（候选，见 §6） | Compaction + 记忆写入 | 压缩前自动写 memory/YYYY-MM-DD.md |
| 7 | 长期记忆 | Markdown + memory_search + 向量 | 中（文件记忆 + llm-wiki 为主） | 无（MemoryStrategy 明示外部） | MISSING | 外部 | agent runtime（候选，见 §6） | Context 加载 | "记得"写入 memory 文件并可检索 |
| 8 | Dreaming/睡眠整合 | 安装版 2026.3.13 无；上游 2026.7.1 有（opt-in，light/REM/deep）；用户自建 brain skill | 中（自建） | 无 | MISSING（可复用上游官方实现） | 外部 | 用户自建（已是 skill）或复用上游 memory-core | 记忆 | 官方 dreaming sweep 或 brain consolidate 可跑 |
| 9 | Cron/心跳/主动执行 | 177 cron + heartbeat + hooks | **重度**（138 启用） | 无调度服务；Kernel 有 event.observe.v0 + budget hook | MISSING | 外部 | scheduler harness 仓（候选，见 §6） | Kernel Journal 订阅 + agent 循环 | cron 定时触发 Agent 并 announce 回 Feishu |
| 10 | Sub-agent/后台任务 | sessions_spawn + announce | 低（0 run） | 无 | MISSING | 外部 | orchestrator 仓（候选，见 §6） | 多 Run + Kernel | spawn 后台 run 完成后回传 |
| 11 | ACP 外部 Harness | `openclaw acp` bridge | 低-中（wrapper 在） | 无 | MISSING | 外部 | harness 仓 | 协议实现 | Codex ACP 会话可跑 |
| 12 | 审批 | exec-approvals | 低 | **durable approval 生产级** | READY（超配） | Kernel | agent-core | 无 | 已有 /v1/approve |
| 13 | 进度/失败呈现 | Control UI + streaming | 低（Feishu 群够用） | 无 UI；Feishu 回复可用 | PARTIAL | 外部 | connector/呈现仓 | Connector | 延迟回复有"处理中" |
| 14 | Skill 系统 | AgentSkills + ClawHub | **重度**（13+ 共享 + 各 ws） | ResourceRef + 渐进披露设计；无 Skill 实现 | PARTIAL | 外部 | skills 仓 | Context 加载 | SKILL.md 指令进 system prompt |
| 15 | 记忆可信度/去重/防循环 | 官方弱（2026.7.1 Dreaming 有打分晋升）；用户自建 FTS5 去重 | 低-中 | 无 | UNKNOWN | 外部 | agent runtime（候选） | 记忆 | 无重复记忆写入 |
| 16 | 自我进化 | 无（用户靠 skill-engineer-agent cron） | 中 | Evolution Harness + HCR 管线 | READY（超配） | 外部 | agent-core tools（短期） | Kernel | evolution plan/report 生成（已有） |

### 4.1 Kernel 边界核对结论（本次修订）

| 检查项 | 结论 | 证据 |
|---|---|---|
| `session-reset-policy` 是否 Kernel 缺口 | **不是**。Kernel 通用 Session 原语齐备：`SessionTarget`（agent_id+channel+conversation_key）、`get_or_create_session`（创建/复用）、`SessionStatus`、`summarized_until_event_id`+`summary`（压缩边界事实） | `src/domain/mod.rs:273`、`src/journal/sqlite.rs:179` |
| 外部 Session Policy 是否缺失 | **是**。daily/idle reset 策略、重置时间、重置后 Context 装载策略、compaction 策略、memory flush 策略属于外部 Agent Runtime | §1.6 对照 OpenClaw `session.reset` 语义 |
| `context-provider-consumer` 是否 Kernel 原语缺失 | **不是**。`context.prepare.v0` 契约完整且已接线运行（HMAC 证明、artifact ref、配置项、生产路径测试）；缺的是外部 Provider 的真实组装实现（当前 passthrough） | `src/hook/context_artifact.rs`、`src/server/delivery.rs:147-160`、VM :17400 运行中 |
| 是否需在 Kernel 重复建设 | **否**。Context 组装、记忆、压缩策略明示外部（负面宪法 + external-orchestration-boundary） | `docs/architecture/KERNEL_NEGATIVE_CONSTITUTION.md` |

---

## 5. Kernel / 外部责任划分（本次调查确认）

Kernel 持有（现状即如此，无改动）:

```text
身份（Principal）、Scope、Run 生命周期、append-only Journal、
Intent/Decision/Invocation/Receipt、Registry/Snapshot、审批（durable approval）、
审计、健康信号、外部制品 opaque_ref/digest、event.observe.v0 事实流、
Session 身份/状态事实与创建/复用/切换通用原语、context.prepare.v0 hook 契约与绑定校验
```

外部（本次调查确认的空白区，全部在 agent-core 之外建设）:

```text
Context 真实组装（Provider 消费 context.prepare.v0；passthrough → 真实实现）
记忆（MemoryStrategy）
压缩/修剪（CompressionStrategy）
Session 生命周期策略（daily/idle reset、重置时间、重置后 Context 装载）
调度（SchedulerBusinessRule：cron/heartbeat）
Router / Multi-Run Orchestrator
Sub-agent 语义
ACP bridge
多渠道呈现（进度/通知/失败 UI）
```

> 与 `docs/architecture/external-orchestration-boundary.md`、`KERNEL_NEGATIVE_CONSTITUTION.md` 完全一致。本轮未发现需要把上述任何一项拉回 Kernel 的理由。

---

## 6. 推荐仓库边界（候选方案，本轮不创建、不冻结）

**重要降级说明**: 以下仓库划分是**候选边界**，不是冻结决策。先提出一个**凝聚的外部 Personal Agent Runtime 候选边界**——Context、Session、Memory 首先作为该 Runtime 内的模块共存。**只有出现独立部署生命周期、权限边界、数据所有权、多消费者或扩缩容需求时，才拆分为独立仓库。**

原则: 不把外部生态堆回 Kernel 单仓；不创建"超级 Harness"；不让 Router 吞掉 Context/记忆/调度；不把任何一项拉回 Kernel。

### 6.1 候选：一个外部 Personal Agent Runtime

```text
personal-agent-runtime（候选单一仓库，P1 的最小载体）
├── context 模块    （context.prepare.v0 Consumer：AGENTS/SOUL/USER/记忆文件读取、截断、注入）
├── session 模块    （daily/idle reset 策略、重置后 Context 装载）
├── memory 模块     （memory/YYYY-MM-DD.md 读写、MEMORY.md、可选向量检索）
├── compaction 模块 （摘要压缩、压缩前 memory flush 触发）
└── agent loop 入口  （调用 Kernel Run API + LLM 循环，Feishu 对话闭环）
```

拆分触发条件（满足其一再拆仓）:

```text
独立部署生命周期（可单独启停/升级）
权限边界（需要独立的 grants/principal 域）
数据所有权（独立数据目录/数据库归属）
多消费者（多个 Agent/Runtime 共享同一服务）
扩缩容（需要独立水平扩展）
```

### 6.2 现有生态仓库归属（保持现状）

| 仓库 | 内容 | 状态 |
|---|---|---|
| `agent-core` | Kernel + 参考 harness + 契约文档 | 现状保留 |
| `agent-forum` / `auth-service` / `svc-workflow` / `svc-okr` / `workflow-todo` / `llm-wiki` / `article-review-platform` / `it-ops-control-plane` | 已分家服务 | 保持独立 |
| `agent-kanban` | OpenClaw 内容备份仓 | 保持独立 |
| 跨仓库契约 | 建议后续版本化（`agent-ecosystem-docs` 或并入已有文档仓） | 候选 |

### 6.3 不做什么

不创建"超级 Harness"；Router、scheduler、sub-agent、ACP 在 P1 不启动；不冻结任何新仓库名。

### 6.4 EXTERNAL_REPO_INVENTORY 完整性声明

```text
EXTERNAL_REPO_INVENTORY_COMPLETE=PARTIAL
```

已确认: 本机 `~/workspace/project/` 下全部目录 + agent-core 主仓 + 已知 GitHub 组织（mayf3）仓库。
未确认范围: （1）其他主机/服务器上可能存在的仓库（svc-workflow 的 canary remote 服务器上可能有未枚举服务）；（2）GitHub 组织中未被本地 clone 的仓库；（3）`~/workspace/deploy/` 等部署目录中的未版本化组件（如 article-review-canary）。完整枚举需要 GitHub org 级 API 盘点 + 服务器清单核对，本轮未执行。

---

## 7. 分阶段替代路线（修订版）

### 7.1 候选顺序比较（证据，非审美）

| 候选顺序 | 用户可见收益 | 前置依赖 | 能减少多少 OpenClaw 使用 | 风险 |
|---|---|---|---|---|
| A. 主 Agent 体验 + Context 加载 | 一个 Feishu 群里的 Agent 有 SOUL/AGENTS 人格与规则 | Kernel+Connector+真实流量已就绪；Provider 需从 passthrough 升级 | 少（单 Agent 场景），但为一切打底 | 低 |
| B. Session/Compaction | 跨日会话连续 + 不爆窗 | 需要 A（Context 组装） | 中 | 中（需要真实长会话验证） |
| C. Memory/Dreaming | 长期记忆 + 夜间整合 | 需要 A+B | 中 | 中（用户已有 brain skill，需对齐；上游官方 Dreaming 可复用） |
| D. Multi-Agent Router | 79 群路由迁移 | 需要 A（每 Agent 上下文）+ 外部编排 | 大（入口形态迁移） | 高（迁移面大） |
| E. 后台任务/外部 Harness 调度 | 138 cron 迁移 | 需要 A + 外部 agent 循环 | 大（运行量主体） | 高（任务语义多、失败面广） |

### 7.2 推荐顺序（修订）

**Phase 1 = 真实纵向切片（不是三个基础设施组件）**:

```text
目标: 迁移一个真实 OpenClaw Agent 到 Agent Core + 外部 Runtime，
      连续可用，并实际减少 OpenClaw 流量。
```

P1 至少覆盖（全部通过外部 Agent Runtime 实现，Kernel 零改动）:

```text
固定 Agent 身份（一个真实 Agent 的 agent_id/workspace/grants 固化）
AGENTS.md / SOUL.md / USER.md 加载（context.prepare.v0 Provider 真实组装）
今日、昨日和长期记忆加载（memory/YYYY-MM-DD.md + MEMORY.md 读取）
Session 创建与跨日消息（同一 session 连续对话）
动态压缩（摘要 + 持久化）
压缩前 memory flush（静默 turn 写 memory 文件）
旧 Session 可追溯（历史 transcript 保留可查）
飞书真实对话（复用已存在的 Feishu→Kernel 链路）
现有外部工具调用（至少一个用户真实在用的工具/服务）
明确回滚到 OpenClaw（恢复 bindings 指向、无数据迁移）
```

**推荐 Canary**（替代原"第一条真实飞书流量"表述）:

```text
选择低风险但真实使用的 Agent（建议从 79 个中选 1 个，如购物清单管家或类似低频高确定性 Agent）
连续使用 7 天
该 Agent 相关消息不再经过 OpenClaw（bindings 迁移到 Agent Core 侧）
记录: 功能缺口、失败次数、人工切回次数
通过标准: 7 天内人工切回 ≤ 2 次且无不可恢复数据丢失
```

**P1 明确不包含**: Router、复杂 Dreaming、全部 cron、sub-agent、ACP。

**为什么 P1 是纵向切片而非 Router**（证据）:
1. 用户每个 Agent 的日常 = "Feishu 群里有一个读 SOUL.md/AGENTS.md、有记忆文件、跨日重置会话的 Agent"——单 Agent 纵切片是 79 个 Agent 共享的最小公分母；
2. 技术底座已齐（Kernel + Feishu Connector + 真实流量 + `context.prepare.v0` 契约已接线），只需把 Provider 从 passthrough 升级为真实组装 + 外部 Runtime 的 session/memory/compaction 模块；
3. Router 在单 Agent 未验证前迁移会导致每个群的人格/规则丢失且无法定位问题；
4. 收益可立即验收：一个真实 Agent 的完整体验闭环 + 可量化减少的 OpenClaw 流量。

**ROUTER_IS_PHASE_1 = false**。

### 7.3 后续阶段（P2 起，同构模板）

```text
P2 多 Agent 横向复制（Router + Multi-Run Orchestrator）: 用外部编排把 P1 验证的
   Agent 体验复制到多个群；Canary: 2 群 2 Agent 无串话。
P3 调度（cron/heartbeat）: 迁移首批 cron（每日学习/简报类）；Canary: 1 个 cron 任务
   定时触发 + Feishu announce + 失败可查。
P4 记忆/知识沉淀增强（Dreaming 接入或对齐 brain-memory-system）: Canary: 官方 dreaming
   sweep 或 brain consolidate 等价物可跑且不污染 MEMORY.md。
P5 sub-agent / ACP: 按用户实际需求再评估（当前 sub-agent 0 run，优先级低）。
```

每阶段模板字段（P1 已在 §7.2 展开；P2+ 复用同构字段）:

```text
用户可见收益 / 建设内容 / 所属外部仓库（候选）/ 依赖 / 不做什么 /
真实 Canary / 完成后能否减少 OpenClaw 使用 / 回滚方式
```

---

## 8. 每阶段 Canary（汇总，修订版）

| 阶段 | Canary | 通过标准 |
|---|---|---|
| P1 真实纵向切片 | **单 Agent 7 天连续使用**（Feishu 真实对话，消息不再经过 OpenClaw） | 7 天人工切回 ≤ 2 次；AGENTS/SOUL 生效；跨日新 session；memory 落盘；压缩后历史可查；无不可恢复数据丢失 |
| P2 Router | 2 群 2 Agent | 群 A 只答 A，群 B 只答 B，无串话 |
| P3 调度 | 1 个 cron 任务 | 定时触发 + Feishu announce + 失败可查 |
| P4 记忆/Dreaming | 记忆检索 + 整合 | memory_search 命中 + 官方 dreaming 或 brain consolidate 等价物可跑 |
| P5 ACP/Harness | Codex ACP 会话 | 通过 agent-core 起 Codex 会话 |

---

## 9. 风险与不确定项

1. **双版本基线差距**: 用户装 2026.3.13，上游 2026.7.1 已新增官方 Dreaming 等。升级评估（是否值得先升级 OpenClaw 再迁移）未做，需用户决策。
2. **cron 任务语义深**: 138 个启用任务里很多带项目路径（如 llm-wiki 编译、repo 扫描），迁移需要任务级定义文件，不能只搬调度器。
3. **bindings 迁移面大**: 79 群路由 + 每 Agent 独立 auth/workspace/skills，迁移是数据/配置工程问题，不是协议问题。
4. **Kernel 现有流量是 canary 型**: 107 条 Feishu run 含 12 Failed / 15 WaitingDispatch，稳定性尚未经受主 Agent 体验验证；P1 Canary 会首次把它当主路径用，需要准备回滚（恢复 bindings 指向 OpenClaw）。
5. **acpx/ACP wrapper 现状未验证**: 只有 wrapper 文件存在，未确认当前在用。
6. **adc-v2 / architecture-portal 未版本化**: 若作为生态一部分，需要先版本化，否则是单点。
7. **UNKNOWN 项**: 记忆去重/防循环在 OpenClaw 官方 2026.3.13 是弱的；2026.7.1 Dreaming 有打分晋升但无完整防召回循环；用户自建方案也缺——这块需要设计验证而非直接复制。
8. **EXTERNAL_REPO_INVENTORY 为 PARTIAL**: 服务器端与 GitHub org 级仓库未完整枚举（§6.4）。

---

## 10. 明确不复制的 OpenClaw 能力

```text
- 多渠道客户端全家桶（macOS app / Web UI / Nodes）：用户只用 Feishu，不做
- 内存中 cache-ttl pruning 的精确语义：先不做，等真实长会话数据再定
- 官方 memory-search 的 provider 自动选择链：用最小实现替代
- 会话 JSONL 转储格式：Kernel 有自身 Journal 事实，不复制 OpenClaw transcript 格式
- 每 Agent 独立 auth-profile 文件体系：用 Kernel Principal/Scope 原语替代
- 不把官方 Dreaming 的完整实现复制进 Kernel：可作为外部组件复用上游或对齐自建 brain 系统
```

---

## 11. 第一阶段建议（不实施）

**Phase 1 = 真实纵向切片**：迁移一个真实 OpenClaw Agent（固定身份 + 上下文文件 + 今日/昨日/长期记忆 + Session 跨日消息 + 动态压缩 + memory flush + 旧 session 可追溯 + Feishu 真实对话 + 外部工具 + 明确回滚）到 Agent Core + 外部 Personal Agent Runtime，7 天 Canary 验证实际减少 OpenClaw 流量。Router 明确**不是**第一阶段。本 PR 只提交调查文档（含本次修订），实施等用户审阅路线图后进行。

---

## 12. 输出汇总（修订版）

```text
USER_INSTALLED_OPENCLAW_VERSION=2026.3.13
CURRENT_UPSTREAM_OPENCLAW_VERSION=2026.7.1 (tag v2026.7.1, SHA 2d2ddc43d0dcf71f31283d780f9fe9ff4cc04fe4; npm latest=2026.7.1-2)
DREAMING_INSTALLED_VERSION_STATUS=absent (2026.3.13 dist/ and memory-core have no dreaming module)
DREAMING_UPSTREAM_STATUS=present (2026.7.1 memory-core, opt-in disabled by default, light->REM->deep, docs/concepts/dreaming.md)
DREAMING_USER_CUSTOM_STATUS=present (self-built brain-memory-system skill, sleep replay/consolidation, not cron-scheduled)

SESSION_RESET_ACTUAL_USER_BEHAVIOR=daily reset at 4AM gateway-local (version default; user did not configure session.reset explicitly; dmScope=per-channel-peer explicit)
SESSION_RESET_UPSTREAM_DEFAULT=daily mode default-enabled, atHour=4 default value; optional idleMinutes sliding window; /new /reset manual
SESSION_HISTORY_PRESERVED=true (reset creates new sessionId; old transcripts retained, maintenance pruneAfter=30d default)

FEISHU_REAL_TRAFFIC_EXISTS=true (107/108 runs from Feishu in live kernel db 2026-07-22..31, incl. run_bd2fc177c7504c7ca0848df77d6d17c9, run_27d0098144504011a678c446521b6251; 96 IngressAccepted)
PHASE_1_FIRST_REAL_TRAFFIC=false (Feishu->Kernel traffic already real)
PHASE_1_FIRST_REPLACEMENT_SLICE=true (P1 = first vertical slice replaceable from OpenClaw)

KERNEL_SESSION_PRIMITIVE_GAP=none (SessionTarget/get_or_create_session/SessionStatus/summary exist as generic governance primitives)
EXTERNAL_SESSION_POLICY_GAP=present (daily/idle reset strategy, reset time, post-reset context loading, compaction & memory flush policy are external Agent Runtime responsibilities)
KERNEL_CONTEXT_CONTRACT_GAP=none (context.prepare.v0 contract wired & running; Provider real assembly logic missing externally)

PHASE_1_TARGET_AGENT=one low-risk real-use agent selected from the 79 (candidate: shopping-list-agent class; final selection during P1 planning)
PHASE_1_USER_VISIBLE_OUTCOME=one real Feishu agent with persona/context/memory/session continuity running on Agent Core + external Runtime, its messages no longer through OpenClaw
PHASE_1_SEVEN_DAY_CANARY=7-day continuous use; record gaps/failures/manual-switch-backs; pass if manual switch-back <= 2 and no unrecoverable data loss
OPENCLAW_USAGE_REDUCED_AFTER_P1=true (single-agent slice; 78 agents remain on OpenClaw)

REPO_SPLIT_FROZEN=false (personal-agent-runtime candidate boundary; split only on deploy/perm/data/multi-consumer/scaling triggers)
EXTERNAL_REPO_INVENTORY_COMPLETE=PARTIAL (unconfirmed: server-side repos, uncloned org repos, ~/workspace/deploy unversioned components)
IMPLEMENTATION_STARTED=false
CODE_CHANGED=false
SERVICE_CHANGED=false
FIRST_BLOCKER=none new; P1 canary must define rollback (restore bindings to OpenClaw) before switching any real agent
```

---

## 附录 A：本次修订变更记录

| 修订点 | 原结论 | 修订后结论 |
|---|---|---|
| 版本基线 | 仅 2026.3.13 | 双基线：用户 2026.3.13 + 上游 2026.7.1（固定 SHA） |
| Dreaming | "OpenClaw 官方没有 Dreaming" | 安装版无；上游 2026.7.1 有（opt-in）；用户夜间做梦 = 自建 brain skill |
| Session reset | "默认每天 4AM 清空 session" | 区分版本默认（daily 模式默认启用，atHour=4 为默认值）/ 用户未显式配置 / reset=新建 sessionId 不删历史 / 旧 transcript 保留可查 |
| Feishu 事实 | "无生产 Feishu 流量，P1 引入首条真实流量" | Feishu→Kernel 真实链路已存在（107/108 runs）；P1 是"首条可迁走的真实纵向切片" |
| Kernel 边界 | `session-reset-policy` 列为 Kernel 缺口 | Kernel Session 通用原语齐备；缺口在外部 Session Policy；`context.prepare.v0` 契约已存在且接线，缺外部 Provider 真实组装 |
| Phase 1 | 三个基础设施组件 | 真实纵向切片（单 Agent 7 天 Canary，实际减少 OpenClaw 流量） |
| 仓库规划 | 冻结 4 个新仓库 | 降级为候选；先凝聚 personal-agent-runtime，按触发条件再拆仓 |
| 盘点完整性 | COMPLETE | PARTIAL + 未确认范围清单 |
