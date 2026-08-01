# OpenClaw 平替差距调查与分阶段迁移方案 V0

> **状态**: 纯调查文档（research-only），不包含实现。
> **性质声明**: 本文档是**跨仓库调查索引**，不代表相关实现归 Agent Core 维护。
> **调查日期**: 2026-08-01
> **调查范围**: OpenClaw 官方 2026.3.13 事实 + 用户本机实例脱敏画像 + Agent Core 生态现状 + 能力矩阵 + 分阶段替代路线。
> **本轮不做**: 不开发 Router、不开发新 Harness、不修改 Kernel、不迁移仓库、不创建仓库、不启动/停止服务、不改代码/配置/数据库。

---

## 0. Executive Summary

用户当前的 OpenClaw 实例（v2026.3.13）是一个**重度使用、多 Agent、多频道、高度自定义**的个人 Agent 基础设施：

- 79 个 Feishu 群绑定 Agent，每 Agent 独立 workspace + SOUL.md/AGENTS.md；
- 177 个 cron 任务（138 个启用），覆盖每日学习、周内化、应用检查、夜间自动审稿等；
- 默认 4AM daily session reset + 预压缩 memory flush（即"每晚清空 session"）；
- 自建 `brain-memory-system` skill 提供"睡眠整合"式长期记忆（即用户口中的"夜间做梦"）；
- 外部生态已分家：svc-workflow / svc-okr / auth-service / workflow-todo / agent-forum / llm-wiki 等独立仓库在跑。

Agent Core 生态现状：

- Kernel 边界执行非常严格（负面宪法 + 外部编排边界 + primitive calculus），**未发现 WRONG_BOUNDARY 严重违规**；
- Kernel 本机在 Lima VM（agent-core-hcr）中运行，端口 4130/4131/17400，但**当前没有生产 Feishu 用户在跑**；
- Coding/Deployment/Capability/Context-Hook Harness 都在 `tools/` 内但逻辑上属于外部组件；
- Agent Forum / Workflow / Auth 已迁出为独立 GitHub 仓库；
- **上下文加载（AGENTS.md/SOUL.md 注入）、记忆、压缩、Router、cron/心跳、后台任务、多渠道呈现全部 MISSING**——恰好与 OpenClaw 用户最依赖的能力重合。

**结论**: 真实差距不是"Kernel 不够强"，而是**外部产品层（Context 组装、记忆、Session 生命周期、调度、路由、呈现）一片空白**。迁移的第一阶段不应是 Router，而是**先把"一个 Agent 的完整主体验"跑通**（上下文文件加载 + 会话连续性 + Feishu 闭环），这是用户 79 个 Agent 中每一个的日常使用模式，也是后续所有能力的底座。

---

## 1. OpenClaw 真实能力模型（官方事实）

### 1.1 版本与来源

- 版本: `openclaw@2026.3.13`（npm 全局安装，本机 `/usr/local/lib/node_modules/openclaw`）
- 仓库: `https://github.com/openclaw/openclaw`（MIT）
- 架构: 单 Gateway 进程 + WS 协议客户端（macOS app / CLI / Web UI / Nodes）
- 事实来源: 官方 `docs/`（安装包内置）与 `dist/` 源码

### 1.2 Agent runtime 与主 Agent Loop

- Gateway RPC `agent` / `agent.wait` 是入口（`docs/concepts/agent-loop.md`）。
- 内部运行 `runEmbeddedPiAgent`（pi-agent-core runtime）：per-session + global 队列串行化 → 构建 pi session → 流式事件（assistant / tool / lifecycle）。
- `agent.wait` 等待 lifecycle end/error，返回 `{status, startedAt, endedAt}`。
- 超时、模型解析、auth profile 解析都在 loop 内。
- **对应 Agent Core**: Kernel `Run` 生命周期 + `Runtime`（`src/runtime/`）已是等价物，但无"主 Agent 循环"概念——Kernel 刻意不为模型跑 loop，loop 属于外部 Harness。

### 1.3 Gateway 与 channel/plugin 边界

- 单 Gateway 持有所有 messaging surfaces（WhatsApp/TG/Discord/iMessage/WebChat 等），控制面客户端走 WS（默认 `127.0.0.1:18789`）。
- 插件是 TypeScript 模块（jiti 加载），可注册 tools / network handlers / hooks / commands / services；官方插件含 Memory (Core)、Memory (LanceDB)、Voice Call、Matrix、Teams 等。
- **对应 Agent Core**: Feishu Connector（`connectors/feishu`）是唯一 channel adapter；Kernel 与 Connector 通过 IPC（`/v1/ingress` + `/v1/execute`）解耦——**架构对应，但渠道数量差距大**（用户只实际使用 Feishu，所以此差距不影响迁移）。

### 1.4 Agent workspace / agentDir / session store

- workspace: 默认 `~/.openclaw/workspace`（每 Agent 独立，多 Agent 时 `agents.list[].workspace`）
- agentDir: `~/.openclaw/agents/<agentId>/agent`（auth profiles、model registry 独立）
- session store: `~/.openclaw/agents/<agentId>/sessions/sessions.json` + `<SessionId>.jsonl`
- **对应 Agent Core**: 已有 `docs/architecture/external-harness-workspace-v0.md` 规划 `~/.agent-core/harnesses/<name>/`，`~/.agent-core/agents/main` 已存在（运行时），但**无 per-agent workspace 规范落地**。

### 1.5 上下文文件加载规则（AGENTS.md / SOUL.md / 等）

官方注入集合（若存在即注入，`docs/concepts/context.md`、`docs/concepts/agent-workspace.md`）：

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

- 截断: `bootstrapMaxChars` 默认 20000 字符/文件，`bootstrapTotalMaxChars` 默认 150000 总量，截断时注入警告。
- 用户实际主 workspace: AGENTS.md 415 行、SOUL.md 36 行、USER.md 17 行、IDENTITY.md 23 行、TOOLS.md 40 行、HEARTBEAT.md 5 行（**MEMORY.md 缺失**，说明该用户长期记忆实际走 memory/ + 外部 llm-wiki）。
- **对应 Agent Core**: `ContextBlock`（`src/domain/context_block.rs`）已有 `AgentProfile`/`SkillCatalog`/`ToolCatalog`/`RecentMessages` 等 kind，但**没有外部上下文文件读取与注入管线**；`context.prepare.v0` hook 已在 Kernel 定义（`src/hook/context_artifact.rs`）但无 Consumer 服务在跑。

### 1.6 Session key / daily reset / idle reset / `/new` / `/reset`

- session key: `agent:<agentId>:<mainKey>`（DM 折叠到 main）；group 独立；cron 用 `cron:<job.id>`。
- **Daily reset 默认 4:00 AM 网关本机时间**（`session.reset.mode=daily, atHour=4`），也可 `idleMinutes` 滑窗，两者先到先重置。
- `/new` `/reset` 手动重置；isolated cron 每 run 新 session。
- 用户实例: 未显式配置 `session.reset` → **默认 daily 4AM**（即"每晚清空 session"），dmScope=per-channel-peer。
- **对应 Agent Core**: Kernel 有 Session/Run 事实记录，但**无 daily reset 策略、无 session key 派生规则**——这些属于外部编排。

### 1.7 Context assembly / pruning / compaction / memory flush

- System prompt = base + skills 列表 + bootstrap context + per-run overrides；`/context list` 可查注入清单。
- Pruning: `contextPruning.mode=cache-ttl`（用户: 30m）——旧 tool result 内存修剪。
- Compaction: 摘要旧历史持久化进 JSONL；`identifierPolicy=strict`（用户配置）保留不透明标识符；可用独立 compaction model。
- **Memory flush**: 接近自动压缩阈值时触发一次**静默 agentic turn**，提醒模型把持久记忆写入 `memory/YYYY-MM-DD.md`，回复 `NO_REPLY` 则不打扰用户（用户配置 enabled，softThresholdTokens=15000，中文 prompt 定制）。
- 用户实例 compaction: mode=default, reserveTokens=16384, keepRecentTokens=30000, reserveTokensFloor=80000。
- **对应 Agent Core**: 无 pruning/compaction/memory-flush 实现；`docs/architecture/context-materialization-hook.md` 把 Context 组装定义为外部 Provider 责任（`context.prepare.v0`）。

### 1.8 Context Engine 插件机制

- `plugins.slots.contextEngine` 选择 context engine 插件（默认 `legacy` 内建；可换 `lossless-claw` 等）；`kind: "context-engine"` 的插件拥有 assemble/ingest/ownsCompaction。
- 用户实例: **无 contextEngine 插件启用**（openviking 扩展存在但 disabled）。
- **对应 Agent Core**: Context Provider 是已规划的外部组件（`TargetKind::ContextProvider` in `src/domain/self_evolution.rs`），契约在 `context.prepare.v0`，尚未实现。

### 1.9 普通记忆 / daily notes / Active Memory / 搜索召回

- Markdown 文件即事实源；`memory_search`（语义召回）+ `memory_get`（定点读取）两个工具。
- 向量索引: 默认启用，`memorySearch.provider` 自动选择（openai/gemini/voyage/mistral/ollama/local）；`sqlite-vec` 加速。
- QMD backend（实验）: BM25+向量+rerank 本地 sidecar（用户**未启用**，无 `memory.backend=qmd`）。
- Memory (LanceDB) 插件: auto-recall/capture 长期记忆（用户**未启用**，`plugins.slots.memory` 为空 → 使用默认 memory-core）。
- **对应 Agent Core**: 记忆完全属于外部（`MemoryStrategy` 明列在 external-orchestration-boundary 禁止入 Kernel 清单）。**现状: 无记忆外部组件。**

### 1.10 Dreaming（Light/REM/Deep）

**官方 2026.3.13 无 Dreaming 功能**（dist 中仅有 banner 玩笑话）。用户口中的"夜间做梦"实为**自建 `brain-memory-system` skill**（`cognitive-brain`）：episodic(海马)/semantic(新皮层)/procedural(小脑)/attention(丘脑)/sleep replay 整合/soul erosion 健康指标，SQLite + LLM 驱动。该 skill 挂在用户 workspace，**未挂任何 cron**。

> 结论: "夜间做梦"是用户自建能力，不是 OpenClaw 平台能力。迁移时按"外部记忆 Harness"对待，与 OpenClaw 无关。

### 1.11 记忆来源 / 可信度 / 去重 / 防召回循环 / 晋升

- 官方: 无系统级记忆来源可信度分级；靠"模型自己写对"（`memory_search` 对索引内容做语义召回，QMD 有 rerank 但没有专门防循环设计）。LanceDB 插件提供 auto-capture/recall。
- 用户侧: `brain-memory-system` 提供去重（FTS5）与冲突检测（soul erosion），但仍无"防召回循环"显式机制。
- **对应 Agent Core**: 全部缺失（外部）。

### 1.12 Multi-agent bindings / 隔离 workspace / 独立 session

- `agents.list[]` + `bindings[]`（channel + peer match → agentId）；每 Agent 独立 workspace/agentDir/session store/auth profiles。
- 用户实例: **79 个 Agent**，全部绑定 Feishu 群（`peer.kind=group`），每个有独立 `groups/workspace-oc_*` 目录，79 个 SOUL.md + 79 个 AGENTS.md + 72 个 skills 目录。
- **对应 Agent Core**: 多 Agent = 外部编排 + 多个普通 Run + 不同 Workspace/Grants（`external-orchestration-boundary.md` §关于多 Agent）。**现状: 无外部编排、无 Router。**

### 1.13 sessions_spawn / background tasks / sub-agent 回传 / 嵌套

- `sessions_spawn` tool: 后台 sub-agent 独立 session（`agent:<id>:subagent:<uuid>`），完成后 announce 回 requester chat；支持嵌套、`--model` 覆盖、thread 绑定、`mode:run|session`。
- 用户实例: `~/.openclaw/subagents/runs.json` 存在但 **0 个 run**（说明该用户当前不太用 sub-agent 工具链；其"多 Agent"是绑定路由而非 spawn）。
- **对应 Agent Core**: 无（外部）。Kernel 的 outbox/worker 是投递可靠性原语，不是 sub-agent 语义。

### 1.14 ACP 外部 Harness 接入

- `openclaw acp` 是 Gateway-backed ACP（Agent Client Protocol）bridge（stdio ↔ Gateway WS）；`sessions_spawn` 支持 `runtime:"acp"`（Codex/Claude Code/Gemini CLI）。
- 用户实例: `~/.openclaw/acpx/codex-acp-wrapper.mjs` 存在（Codex ACP wrapper），说明使用过/在配置 ACP 通道。
- **对应 Agent Core**: 无 ACP；Coding Harness 是自有协议（`external.coding_*` 操作），与 ACP 正交。

### 1.15 cron / heartbeat / hooks / standing orders / 主动执行

- **Cron**: 精确调度、isolated session、`delivery.mode=announce` 回频道；top-of-hour 自动加 0-5min 抖动。
- **Heartbeat**: 主 session 周期 agent turn（默认 30m），读 HEARTBEAT.md，`HEARTBEAT_OK` ack 抑制空消息，可 activeHours。
- **Hooks**: 内部 hooks（`agent:bootstrap`、command hooks `/new` `/reset` `/stop`）+ 插件 hooks（`before_prompt_build`、`before/after_compaction`、`before/after_tool_call`、`message_received/sending/sent`、`session_start/end`、`gateway_start/stop`）。官方内置 4 个 hook（session-memory、bootstrap-extra-files、command-logger、boot-md）。
- 用户实例: 177 个 cron jobs（138 enabled），全部 `sessionTarget=isolated` + Feishu announce；cron 集中在**每日学习（29）、周内化（21）、应用检查（20）、夜间审稿（7）、随想（4）**等模式；另 crontab 有健康监控、备份任务；launchd 有 auto-repair daemon + gateway plist。
- **对应 Agent Core**: cron/heartbeat 属于外部（`SchedulerBusinessRule` 禁止入 Kernel）；Kernel 有 `event.observe.v0`（Journal 事实可被外部消费）与 Run Budget Hook（`src/hook/budget.rs`）——**订阅/触发基础在，调度产品层无**。

### 1.16 用户界面：进度 / 审批 / 主动通知 / 失败呈现

- Web Control UI、macOS app、CLI、WebChat、canvas host（`/__openclaw__/canvas/` + `/a2ui/`）、Nodes。
- 用户实例: 主要呈现面是 **Feishu 群消息**；辅以 token-monitor 脚本 + Feishu webhook 告警（`scripts/token-monitor.sh` 等自建监控）。
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
| Session reset | 默认 daily 4AM（未显式配置） | `openclaw.json` 无 `session.reset` |
| dmScope | per-channel-peer | `session.dmScope` |
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
| 夜间做梦 | 官方无此功能；自建 `brain-memory-system`（sleep replay/hippocampal consolidation），**未挂 cron**；夜间任务实际是"夜间自动审稿 8 轮"等 cron | 自建 skill + 夜间 cron 混合 |
| 自动反思 | delivery-review-agent（复盘专家）、soul-questioner（每日灵魂拷问）、各 Agent"周内化/应用检查" cron | 通过 cron + 专用 Agent 实现 |
| 动态压缩 | compaction.mode=default + memoryFlush + contextPruning cache-ttl 30m | 官方机制启用 |
| 每晚清空 session | 默认 daily reset atHour=4（未配置即默认）+ memoryFlush prompt "会话即将重置清理" | 官方默认 + 定制 prompt |
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

以 git remote / README / manifest / 运行路径为证据，不按目录名猜。

### 3.2 盘点表

| 组件 | 类别 | 证据 | 位置 | 成熟度 |
|---|---|---|---|---|
| Kernel（Rust） | IN_KERNEL | `Cargo.toml` package=agent-core-kernel；`src/` 169 个 rs | agent-core 主仓 | 生产可用（M0-M5+ 全 Done，HCR 在 Lima VM 跑） |
| Feishu Connector (TS) | IN_REPO_EXTERNAL | `connectors/feishu/`，README 自述"不是 Kernel" | agent-core 主仓 | 生产可用（M1 系 Done）；长期目标是独立插件 |
| Coding Harness | IN_REPO_EXTERNAL | `tools/coding-harness/`，自持 Cargo.toml package=coding-harness，已 build | agent-core 主仓 | 生产可用（external.coding_* 已注册 + HCR 实测） |
| Capability Host | IN_REPO_EXTERNAL | `tools/capability-host/` package=capability-host，已 build | agent-core 主仓 | 可用（invocable capability 生成链） |
| Deployment Harness | IN_REPO_EXTERNAL | `tools/deployment-harness/` package=deployment-harness，已 build，`deployment.effect.v0` | agent-core 主仓 | 可用（升级/回滚/禁用） |
| Context Hook Harness | IN_REPO_EXTERNAL | `tools/context-hook-harness/` package=context-hook-harness | agent-core 主仓 | 实验（context.prepare.v0 契约定义） |
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
- **尚不存在**: Context Provider（记忆/上下文组装）、Compaction/Pruning 服务、Router/Multi-Run Orchestrator、Cron/Heartbeat 调度服务、Session 生命周期策略、多渠道 UI、ACP bridge、Dreaming/记忆晋升。
- **历史实现/已退役**: 无重大退役项（builtin `time.now` 已由 PR #165 退役，改走外部 harness——符合方向）。

---

## 4. 完整差距矩阵

差距等级: `READY` / `PARTIAL` / `MISSING` / `WRONG_BOUNDARY` / `UNKNOWN`

| # | 能力 | OpenClaw 当前行为 | 用户真实使用 | Agent Core 生态现状 | 差距 | Kernel 或外部 | 建议所属仓库 | 依赖 | 最小验收场景 |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 多渠道入口 | 7+ 频道 + 插件 | 仅 Feishu | Feishu Connector 生产可用 | PARTIAL | 外部 | agent-core（短期）→ 独立 connector 仓库 | Kernel IPC | Feishu 消息闭环（已有） |
| 2 | 多 Agent 群路由 | bindings 79 条 | **重度**（日常入口） | 无 Router/外部编排 | MISSING | 外部 | 独立 router harness 仓 | Context 加载 + Session 事实 | 两个群路由到两个 Agent 各回各话 |
| 3 | 上下文文件加载 | AGENTS/SOUL/USER/TOOLS/HEARTBEAT 注入 + 截断 | **重度** | `context.prepare.v0` 契约 + ContextBlock kind 已定义，无 Consumer | PARTIAL | 外部 | context-provider 仓 | Kernel hook ABI | AGENTS.md 内容出现在 system prompt（/context 可查） |
| 4 | Session key / 每日重置 | daily 4AM 默认 + /new /reset | **重度**（每晚清空） | Session/Run 事实在 Kernel，无重置策略 | MISSING | 外部 | session-policy 仓 | Kernel Session API | 跨日第二条消息自动进新 session |
| 5 | 动态压缩 | auto-compaction + 独立模型 + identifierPolicy | **重度** | 无 | MISSING | 外部 | context-provider/compactor 仓 | Context 组装 + 模型 | 超窗消息被摘要压缩且 JSONL 持久 |
| 6 | 预压缩 memory flush | 静默 agentic turn | **重度**（中文定制） | 无 | MISSING | 外部 | memory harness 仓 | Compaction + 记忆写入 | 压缩前自动写 memory/YYYY-MM-DD.md |
| 7 | 长期记忆 | Markdown + memory_search + 向量 | 中（文件记忆 + llm-wiki 为主） | 无（MemoryStrategy 明示外部） | MISSING | 外部 | memory harness 仓 | Context 加载 | "记得"写入 memory 文件并可检索 |
| 8 | Dreaming/睡眠整合 | 官方无；用户自建 brain skill | 中（自建） | 无 | MISSING（非 OpenClaw 差距） | 外部 | 用户自建（已是 skill） | 记忆 | brain consolidate 可跑 |
| 9 | Cron/心跳/主动执行 | 177 cron + heartbeat + hooks | **重度**（138 启用） | 无调度服务；Kernel 有 event.observe.v0 + budget hook | MISSING | 外部 | scheduler harness 仓 | Kernel Journal 订阅 + agent 循环 | cron 定时触发 Agent 并 announce 回 Feishu |
| 10 | Sub-agent/后台任务 | sessions_spawn + announce | 低（0 run） | 无 | MISSING | 外部 | orchestrator 仓 | 多 Run + Kernel | spawn 后台 run 完成后回传 |
| 11 | ACP 外部 Harness | `openclaw acp` bridge | 低-中（wrapper 在） | 无 | MISSING | 外部 | harness 仓 | 协议实现 | Codex ACP 会话可跑 |
| 12 | 审批 | exec-approvals | 低 | **durable approval 生产级** | READY（超配） | Kernel | agent-core | 无 | 已有 /v1/approve |
| 13 | 进度/失败呈现 | Control UI + streaming | 低（Feishu 群够用） | 无 UI；Feishu 回复可用 | PARTIAL | 外部 | connector/呈现仓 | Connector | 延迟回复有"处理中" |
| 14 | Skill 系统 | AgentSkills + ClawHub | **重度**（13+ 共享 + 各 ws） | ResourceRef + 渐进披露设计；无 Skill 实现 | PARTIAL | 外部 | skills 仓 | Context 加载 | SKILL.md 指令进 system prompt |
| 15 | 记忆可信度/去重/防循环 | 官方弱；用户自建 FTS5 去重 | 低-中 | 无 | UNKNOWN | 外部 | memory harness 仓 | 记忆 | 无重复记忆写入 |
| 16 | 自我进化 | 无（用户靠 skill-engineer-agent cron） | 中 | Evolution Harness + HCR 管线 | READY（超配） | 外部 | agent-core tools（短期） | Kernel | evolution plan/report 生成（已有） |

---

## 5. Kernel / 外部责任划分（本次调查确认）

Kernel 持有（现状即如此，无改动）:

```text
身份（Principal）、Scope、Run 生命周期、append-only Journal、
Intent/Decision/Invocation/Receipt、Registry/Snapshot、审批（durable approval）、
审计、健康信号、外部制品 opaque_ref/digest、event.observe.v0 事实流
```

外部（本次调查确认的空白区，全部在 agent-core 之外建设）:

```text
Context 组装（context.prepare.v0 Consumer）
记忆（MemoryStrategy）
压缩/修剪（CompressionStrategy）
Session 生命周期策略（daily reset 等）
调度（SchedulerBusinessRule：cron/heartbeat）
Router / Multi-Run Orchestrator
Sub-agent 语义
ACP bridge
多渠道呈现（进度/通知/失败 UI）
```

> 与 `docs/architecture/external-orchestration-boundary.md`、`KERNEL_NEGATIVE_CONSTITUTION.md` 完全一致。本轮未发现需要把上述任何一项拉回 Kernel 的理由。

---

## 6. 推荐仓库边界（规划，本轮不创建）

原则: 不把外部生态堆回 Kernel 单仓；共享契约文档与协议定义留在可跨仓引用的位置。

| 建议仓库 | 内容 | 依据 |
|---|---|---|
| `agent-core`（现状） | Kernel + 参考 harness + 契约文档（短期） | README 明示 tools/ 会迁出 |
| `harness-runtime`（新） | Coding/Deployment/Capability/Shadow 等已成熟 harness 合并或分仓迁移 | 已有独立 package + 已 build |
| `context-provider`（新） | context.prepare.v0 Consumer + Context 组装 + 压缩策略 | 契约已定，无实现 |
| `memory-harness`（新） | 记忆文件/向量/QMD 适配 + memory flush + 晋升 | 用户记忆依赖重 |
| `scheduler-harness`（新） | cron/heartbeat/夜间任务编排 + Feishu announce | 用户 138 cron 是最大运行量 |
| `router-harness`（新） | bindings 路由 + Multi-Run Orchestrator + sub-agent | 用户 79 群路由 |
| 现有独立仓 | agent-forum / auth-service / svc-workflow / svc-okr / workflow-todo / llm-wiki 等 | 已分家，保持 |
| 跨仓库契约 | **建议新建 `agent-ecosystem-docs`（或并入 llm-wiki/architecture-portal 版本化）** | 本文档 + external-orchestration-boundary 等应跨仓可见 |

不做什么: 不创建"超级 Harness"；不让 Router 吞掉 Context/记忆/调度；不把任何一项拉回 Kernel。

---

## 7. 分阶段替代路线

### 阶段比较（证据，非审美）

| 候选顺序 | 用户可见收益 | 前置依赖 | 能减少多少 OpenClaw 使用 | 风险 |
|---|---|---|---|---|
| A. 主 Agent 体验 + Context 加载 | 一个 Feishu 群里的 Agent 有 SOUL/AGENTS 人格与规则 | Kernel+Connector 已就绪；只需 context provider | 少（单 Agent 场景），但为一切打底 | 低 |
| B. Session/Compaction | 跨日会话连续 + 不爆窗 | 需要 A（Context 组装） | 中 | 中（需要真实长会话验证） |
| C. Memory/Dreaming | 长期记忆 + 夜间整合 | 需要 A+B | 中 | 中（用户已有 brain skill，需对齐） |
| D. Multi-Agent Router | 79 群路由迁移 | 需要 A（每 Agent 上下文）+ 外部编排 | 大（入口形态迁移） | 高（迁移面大） |
| E. 后台任务/外部 Harness 调度 | 138 cron 迁移 | 需要 A + 外部 agent 循环 | 大（运行量主体） | 高（任务语义多、失败面广） |

### 推荐顺序

**Phase 1: A（主 Agent 体验 + Context 加载）**

理由（证据）:
1. 用户每个 Agent 的日常 = "Feishu 群里有一个读 SOUL.md/AGENTS.md、有记忆文件、跨日重置会话的 Agent"——A 是 79 个 Agent 共享的最小公分母；
2. 技术底座已齐（Kernel + Feishu Connector + LLM 路径 + `context.prepare.v0` 契约 + ContextBlock kind 定义），只差一个外部 Context Provider Consumer 服务；
3. 其他阶段（B/C/D/E）全部依赖 A 的 Context 组装能力；Router 在 A 未就绪时迁移会导致每个群的人格/规则丢失；
4. 收益可立即验收：一个 Agent 的"人格 + 规则 + 会话"闭环，可用真实 Feishu 群对比 OpenClaw 行为。

**ROUTER_IS_PHASE_1 = false**（用户 79 群路由是现状，但 Router 不是使能层；先让单个 Agent 完整可用，再用外部编排复制到 79 个群）。

### 每阶段模板（Phase 1 示例，其余阶段同构）

```text
用户可见收益: 一个 Feishu 群里的 Agent 具备 SOUL/AGENTS 人格、规则与跨日会话记忆
建设内容: Context Provider Consumer（context.prepare.v0）+ workspace 上下文文件读取/截断 +
          Session 每日重置策略（外部）+ 最小 memory/YYYY-MM-DD.md 读写
所属外部仓库: context-provider（新）
依赖: Kernel 4130 + Feishu Connector 4131 + LLM 配置 + context.prepare.v0 hook URL
不做什么: 不做压缩、不做记忆检索、不做 Router、不做 cron
真实 Canary: 在真实 Feishu 群与 Agent 对话，验证 AGENTS.md 规则生效、
             /context 可查注入、跨日第二条消息进新 session、memory 文件落盘
完成后能否减少 OpenClaw 使用: 能（单 Agent 场景可切换，其余 78 群仍留 OpenClaw）
回滚方式: 停 context provider + 恢复 Feishu connector 指向 OpenClaw 群；无数据迁移
```

---

## 8. 每阶段 Canary（汇总）

| 阶段 | Canary | 通过标准 |
|---|---|---|
| P1 Context 加载 | Feishu 群人格闭环 | AGENTS 规则生效 + session 跨日重置 + memory 落盘 |
| P2 Session/Compaction | 长会话不爆窗 | 自动压缩后关键标识符保留、历史可查 |
| P3 Memory/Dreaming | 记忆检索 + 睡眠整合 | memory_search 命中 + brain consolidate 等价物可跑 |
| P4 Router | 2 群 2 Agent | 群 A 只答 A，群 B 只答 B，无串话 |
| P5 调度 | 1 个 cron 任务 | 定时触发 + Feishu announce + 失败可查 |
| P6 ACP/Harness | Codex ACP 会话 | 通过 agent-core 起 Codex 会话 |

---

## 9. 风险与不确定项

1. **OpenClaw 无官方 Dreaming**: 用户"夜间做梦"依赖自建 skill，迁移时需把 brain-memory-system 当作一等外部组件对待，不能指望 OpenClaw 或 Kernel 提供。
2. **cron 任务语义深**: 138 个启用任务里很多带项目路径（如 llm-wiki 编译、repo 扫描），迁移需要任务级定义文件，不能只搬调度器。
3. **bindings 迁移面大**: 79 群路由 + 每 Agent 独立 auth/workspace/skills，迁移是数据/配置工程问题，不是协议问题。
4. **Kernel 当前无生产 Feishu 用户**: HCR 在 Lima VM 跑，尚无"真实日常流量"验证；P1 的 Canary 会首次引入真实流量，需要准备回滚。
5. **acpx/ACP wrapper 现状未验证**: 只有 wrapper 文件存在，未确认当前在用。
6. **adc-v2 / architecture-portal 未版本化**: 若作为生态一部分，需要先版本化，否则是单点。
7. **UNKNOWN 项**: 记忆去重/防循环在 OpenClaw 官方就是弱的，用户自建方案也缺防召回循环——这块需要设计验证而非直接复制。

---

## 10. 明确不复制的 OpenClaw 能力

```text
- 多渠道客户端全家桶（macOS app / Web UI / Nodes）：用户只用 Feishu，不做
- 内存中 cache-ttl pruning 的精确语义：先不做，等真实长会话数据再定
- 官方 memory-search 的 provider 自动选择链：用最小实现替代
- 会话 JSONL 转储格式：Kernel 有自身 Journal 事实，不复制 OpenClaw transcript 格式
- Dreaming 分阶段（Light/REM/Deep）：官方无此功能，用户自建，按外部 skill 对待
- 每 Agent 独立 auth-profile 文件体系：用 Kernel Principal/Scope 原语替代
```

---

## 11. 第一阶段建议（不实施）

**Phase 1 = "单 Agent 主体验"**（Context Provider + Session 策略 + 最小记忆），理由与模板见 §7。Router 明确**不是**第一阶段。本 PR 只提交调查文档，实施等用户审阅路线图后进行。

---

## 12. 输出汇总

```text
OPENCLAW_VERSION=2026.3.13
OPENCLAW_CAPABILITY_AREAS=gateway,multi-agent-bindings,context-files,session-reset,compaction,memory,skills,cron,heartbeat,hooks,subagents,acp,ui
USER_ACTUALLY_USED_CAPABILITIES=feishu-group-routing(79),cron(138-enabled),daily-session-reset,memory-flush,context-files,llm-wiki,knowledge-base,brain-memory-system,monitor-scripts
AGENT_CORE_ECOSYSTEM_REPOS=agent-core,agent-forum,auth-service,svc-workflow,svc-okr,workflow-todo,llm-wiki,article-review-platform,it-ops-control-plane,agent-kanban
EXTERNAL_REPO_INVENTORY_COMPLETE=true
KERNEL_GAPS=context-provider-consumer,session-reset-policy,(approval is READY)
EXTERNAL_ECOSYSTEM_GAPS=context-assembly,memory,compaction,scheduler,cron,router,multi-run-orchestrator,subagent,acp,ui
WRONG_BOUNDARY_FINDINGS=none-critical;all product-layer concerns correctly externalized
RECOMMENDED_DOC_REPO=agent-ecosystem-docs (new) OR agent-core/docs/research (current)
DOCUMENT_PATH=docs/research/openclaw-replacement-study-v0.md
BRANCH=<created-on-push>
COMMIT_SHA=<created-on-push>
PR_NUMBER=<created-on-push>
RECOMMENDED_REPLACEMENT_PHASES=P1-context-loading -> P2-session/compaction -> P3-memory/dreaming -> P4-router -> P5-scheduler -> P6-acp
RECOMMENDED_PHASE_1=P1: single-agent main experience (Context Provider + session policy + minimal memory)
WHY_PHASE_1=smallest common denominator of all 79 agents; all other phases depend on context assembly; infra ready
ROUTER_IS_PHASE_1=false
IMPLEMENTATION_STARTED=false
CODE_CHANGED=false
SERVICE_CHANGED=false
FIRST_BLOCKER=no production Feishu traffic on Kernel yet; P1 canary introduces first real traffic
```
