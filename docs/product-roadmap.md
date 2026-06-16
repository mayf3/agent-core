# Agent Core 产品路线图

本文描述 Agent Core 的最终产品形态，以及从当前阶段逐步完成到最终形态的宏观路线。它不是 sprint 施工单。具体实施细节见
[Phase 0 Construction Plan](./phase0-plan.md) 和
[Agent Core Milestones](./milestones.md)。长期协议边界见
[Architecture RFC](./architecture-rfc.md)。

## 产品北极星

Agent Core 的目标是成为一个小而稳定的 Agent Kernel：

```text
用户或外部事件
-> Channel Connector
-> Rust Kernel
-> Context
-> Model
-> Invocation Gateway
-> Adapter / 外部服务
-> Receipt
-> Journal
-> 可观测状态
```

最终产品不是大而全的工作流平台，而是：

- 一个本地优先、可长期运行的 Rust Kernel；
- 一组清晰的 connector / adapter / context / evaluator 扩展面；
- 一条通过 Git、Replay、Eval、PR 推进自我改进的受控路径。

核心原则仍然是：

```text
协议可以提前想清楚；
实现必须等真实失败和重复模式证明后再长出来。
```

## 最终产品形态

### 用户视角

用户可以从飞书手机端、桌面端、CLI 或其他通道和 Agent 通信。一次正常任务应该像这样：

```text
发送消息
-> 看到低干扰的处理中反馈
-> 收到回答或任务结果
-> 延迟时能看到状态
-> 高风险操作需要确认
-> 失败时能重试或追溯
```

产品首先要做到“可靠有用”，再追求复杂能力：

- 稳定聊天和任务闭环；
- 不重复回复；
- 失败状态清楚；
- 不泄露 secret；
- 能接入少量真实外部系统；
- 能通过 PR 方式改进自身代码。

### 运行者视角

运行者得到的是一个可诊断、可修复的小运行时：

- `/health` 可看当前状态；
- SQLite Journal 可追溯事实；
- hash chain 可检测篡改；
- worker/outbox projection 可恢复；
- unknown dispatch 不自动重发；
- connector-local UX 状态不污染核心 Journal；
- runtime 数据默认在 `~/.agent-core`，源代码仓库只放代码和 bootstrap 默认值。

系统状态应该能从 SQLite、健康接口、日志和 Git 历史理解，而不是藏在一个隐形工作流引擎里。

### 开发者视角

开发者通过窄模块扩展能力：

- channel connector；
- model provider；
- context contributor；
- invocation adapter；
- policy / approval rule；
- replay fixture；
- evaluator script。

这些模块可以先在单仓库内验证形状，等边界稳定后再拆成独立包或插件。

### Agent 自身视角

Agent 后期可以受控地改进 harness：

```text
candidate branch / worktree
-> replay selected historical runs
-> evaluator 生成 score.json 和 report.md
-> open PR
-> merge 或 reject
-> tag last-known-good
```

Agent 不直接修改生产运行状态。推广路径必须经过 Git、Replay、Eval、PR。

## 内核边界

Agent Core 拥有：

- run lifecycle；
- session identity；
- run principal 和 capability grant；
- append-only Journal；
- projection repair；
- context assembly contract；
- model provider boundary；
- invocation approval and dispatch；
- receipt / audit semantics；
- health and recovery surfaces。

Agent Core 不拥有：

- 业务 workflow graph；
- 多 Agent 编排 UI；
- 长期记忆产品；
- 项目管理系统；
- 部署平台；
- 数据分析 dashboard；
- 完整 sandbox 产品；
- 飞书业务逻辑。

这些功能可以接入 Kernel，但不应该变成 Kernel。

## 宏观阶段

### Phase 0: Durable Chat Kernel

状态：完成。全部代码要求已落地 `main`，`pnpm check` 全绿。

目标：

```text
Feishu / CLI text
-> Rust Kernel
-> Context
-> LLM
-> durable outbox
-> reply
```

用户可感知结果：

- 可以从飞书和 Agent 对话；
- Agent 走模型回复；
- 重复飞书回复风险被保守控制；
- health 和 recovery 能看见 durable state。

已形成能力：

- SQLite Journal 从第一条消息开始使用；
- worker_jobs / outbox_dispatches 是 projection，不是事实源；
- restart recovery；
- connector-local reaction（含 bounded retry scheduling）；
- `~/.agent-core` 下的 agent data；
- OpenAI-compatible model fallback；
- TypeScript Feishu Connector 仍是边缘 adapter。

实现要点（已落地，非待办）：

- `dispatch_once` loop 已接入 server startup（`start_outbox_dispatcher_loop`，由 `serve()` 启动）；
- `Runtime::deliver` 不再同步发送，改为 `queue_outbox_dispatch` + `update_run_status("WaitingDispatch")`，由独立 dispatcher 消费 `outbox_dispatches`；
- connector-local reaction add/remove 走 bounded `withRetry`（指数退避 + 抖动，非 keepalive loop），保留 Phase 0 "one add and one delete per handled message" 不变式；
- Journal kind decode 的兜底从静默 `RunCompleted` 收紧为 `JournalEventKind::Unknown` 哨兵（见 §当前下一批决策）。

退出标准：

- `pnpm check` 通过；
- secret scan 通过；
- stale / unknown outbox 不会自动重发；
- Feishu Connector 不拥有 Runtime / Gateway / Journal。

### Phase 1: Operational Hardening

目标：让 Phase 0 Kernel 变得“无聊地稳定”。

用户可感知结果：

- 延迟任务能从 health 看见；
- unknown dispatch 明确可见；
- ✅ 有 repair command 或操作手册（见 [Operating Guide](./operating-guide.md)，已落地）；
- ✅ shutdown / restart 行为可预测（restart recovery 在启动时把 abandoned dispatch 按 Journal terminal fact 修对；端到端生命周期测试 `tests/m1_restart_recovery_lifecycle.rs` 锁死 crash→restart→reconcile→idempotent 路径，已落地）。

内核补强：

- ✅ stricter Journal decode（`parse_kind` → `Unknown` 哨兵，已落地）；
- 是否引入 `RunStatus::Unknown` 的明确决策；
- projection verify / repair；
- ✅ migration check（启动时 `PRAGMA user_version` 校验，已落地）；
- ✅ release checklist（见 [Release Checklist](./release-checklist.md)，已落地）；
- ✅ parse / kind 漂移检测（`row_to_event` 在读到未知 kind 时输出脱敏 eprintln，已落地）。

暂不做：

- workflow engine；
- 通用 plugin registry；
- shell tool。

### Phase 2: Invocation Gateway and Safe Tools

目标：让 Agent 能做少量真实工作，但不破坏小内核边界。

用户可感知结果：

- 可以让 Agent 做一个有边界的小动作；
- read-only 操作可以根据 capability grant 执行；
- write / risky 操作需要 approval；
- 每次外部动作都有 intent、decision、receipt、audit。

内核新增：

- tool contract；
- intent schema validation；
- run principal execution profile；
- fixed policy pipeline；
- approval state；
- 第一批低风险本地 adapter。

暂不做：

- Feishu 入口直接获得 shell 权限；
- 模型临时生成未注册工具；
- 部署类 adapter。

### Phase 3: Plugin and Connector Surface

目标：把通道和外部能力增长从 Kernel 中移出去。

用户可感知结果：

- Feishu 可以变成独立 connector 或 plugin；
- model provider 可以通过配置切换；
- 新外部系统可以通过 adapter 接入，而不是改 Runtime。

内核新增：

- connector manifest；
- model provider manifest；
- invocation adapter manifest；
- context contributor manifest；
- trusted local loading rules；
- compatibility version check。

暂不做：

- untrusted in-process code injection；
- 可绕过 final system guard 的动态 user hook；
- 过早拆大量 package。

### Phase 4: Context Modules and Task Memory

目标：通过受控 context 提升 Agent 有用性，而不是把长期记忆产品塞进 Kernel。

用户可感知结果：

- Agent 能记住近期任务上下文；
- agent / project profile 在 `~/.agent-core` 中；
- context block 能解释来源；
- 敏感内容可以被 policy 排除。

内核新增：

- context contributor registry；
- context block metadata；
- token budget planner；
- context snapshot；
- 低风险 block compression。

暂不做：

- opaque vector memory 作为核心依赖；
- agent 可随意改 root system；
- hidden context injection。

### Phase 5: External Workflow and Multi-Agent Integration

目标：支持多 Agent 和外部 workflow，但编排本身仍在 Kernel 外。

用户可感知结果：

- Agent 可以接收外部 workflow 的任务；
- 任务交接可审计；
- 多个 agent profile 可以协作，但权限隔离。

内核新增：

- external system manifest；
- command / query / event / receipt protocol；
- delegation packet；
- agent directory isolation；
- channel-aware capability grant。

暂不做：

- 内置 workflow graph engine；
- 内置多 Agent scheduler；
- agent 之间隐式共享权限。

### Phase 6: Replay, Evaluation, Self-Evolution

目标：让 Agent 能通过证据改进 harness。

用户可感知结果：

- Agent 提交代码变更 PR；
- 选定历史 run 可以 replay；
- evaluator 说明变更是否变好；
- rollback 有 last-known-good tag。

新增能力：

- replay input format；
- context snapshot replay；
- evaluator script contract；
- `score.json` / `report.md`；
- candidate worktree runner；
- PR promotion checklist；
- rollback tag。

暂不做：

- 生产进程自我修改；
- 无 eval 自动合并；
- Git 外的隐藏变更。

### Phase 7: Productization

目标：让另一个开发者或另一个 Agent 能安装、运行、扩展这个系统。

用户可感知结果：

- 安装路径清楚；
- 一组 service 可以启动 Kernel 和 connector；
- 常见故障有 runbook；
- 示例 plugin 展示扩展方式。

新增内容：

- release packaging；
- service templates；
- example connector；
- example read-only adapter；
- example evaluator；
- operator runbook；
- upgrade / migration notes。

## 外部功能清单

这些功能重要，但在重复模式稳定前应保持外置：

- 完整 Feishu connector package；
- operator dashboard；
- project management connector；
- browser automation connector；
- deployment connector；
- richer sandbox runner；
- long-term memory service；
- multi-agent planner；
- workflow graph engine。

Kernel 可以定义协议，但不吸收这些产品逻辑。

## 验收阶梯

| Level | 产品能力 | 必须成立 |
|---|---|---|
| L0 | Chat works | Feishu / CLI 消息能产生 durable reply |
| L1 | Runtime survives restarts | accepted work 和 outbox state 能安全恢复 |
| L2 | Actions are safe | 外部动作都有 intent、approval、receipt |
| L3 | Extensions are clean | connector / adapter 能脱离 Kernel 开发 |
| L4 | Work is replayable | 历史 run 可以基于 snapshot replay |
| L5 | Harness can improve itself | candidate 经过 replay、eval、PR |
| L6 | Product is operable | 安装、health、repair、upgrade 文档清楚 |

## 当前下一批决策

近期优先做内核 hardening，再扩大能力。**已落地**的决策标 ✅，仍未决的保持开放：

- ✅ **Journal kind decode 收紧**：`parse_kind` 的兜底已从静默 `RunCompleted` 改为 `JournalEventKind::Unknown` 哨兵；未知 kind 不再伪装成 run completion，`verify_hash_chain` 仍能检测篡改（PR #44，已合入 `main`）。`parse_kind`/`row_to_event` 刻意保持非 `Result`，以保留现有 `/health` 的 `status:"corrupt"` 语义。
- 是否引入 `RunStatus::Unknown`：仍未决。当前 `unknown` dispatch 用 `WaitingDispatch` + outbox projection + health 表达，阶段 0 是否引入显式 `RunStatus::Unknown` 需要先讨论跨切面影响（runs.status 序列化面、所有 match 点、DB 已有数据兼容）。
- 第一个非 chat 工具选什么；
- Feishu connector 什么时候移出仓库；
- replay fixture format 如何设计。

最终判断标准：

```text
越靠近 Kernel，越保守；
越靠近业务能力，越外置；
越可能产生副作用，越需要 intent、approval、receipt、audit。
```
