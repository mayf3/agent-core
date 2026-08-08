# Agent Core 外部 Harness 边界 V1（冻结）

> **STATUS: Current V1 implementation/history authority.** 本文与当前代码一致，是当前实施/治理基线。
>
> **V2.1 NOTE:** 部分 ownership 决策（Session / Run / Agent Runtime / Approval / Registry-ToolCatalog / Context）正在 [`AGENT_CORE_V2_1_ARCHITECTURE_DISCUSSION_MAP.md`](./AGENT_CORE_V2_1_ARCHITECTURE_DISCUSSION_MAP.md) 中重新审查，该文仍为 WORKING DRAFT。不要静默改写本文以匹配 V2.1；在新的替代边界明确冻结前，保留本文作为 V1 证据。

> 状态：**架构强约束（冻结）**
> 关系：本文在 [`external-orchestration-boundary.md`](./external-orchestration-boundary.md) 与
> [`extension-hook-and-external-harness-boundary-v0.md`](./extension-hook-and-external-harness-boundary-v0.md)
> 之上，**冻结 Run outcome、续跑、Hook 调用方向与 Bootstrap/Self-hosting 推进边界**。
> 与本文冲突的任何修改必须标记 `ARCHITECTURE_CHANGE_REQUIRED=true`，不得伪装成普通修复。
> **每次开发前必须先读取本文档并回答 §1 防跑偏问题。**

---

## 1. 开发前防跑偏问题

开始调查和修改前，必须先用人话回答：

```text
当前北极星是什么？
首个阻塞是什么？
是否能完全放在外部 Agent/Harness 解决？
是否真的需要改 Kernel？
是否引入产品特判或手工绕过？
下一步最小动作是什么？
本阶段目标完成后是否应该停止？
```

预期边界：

```text
北极星：
Kernel 是极简治理内核。
不同外部 Harness 通过窄契约组合，形成持久 Agent 的路由、上下文、工具使用、续跑和协作能力。

首个阻塞：
当前 Run 因预算 yield 后，需要用户手工发送“继续”，
没有外部 Harness 自动在同一 Session 开启下一 Run。

原则：
不增加 task_id、progress、checkpoint、development_job 或产品状态机。
```

---

## 2. 北极星与原则

Kernel 只拥有**通用治理事实**：

```text
Principal
Session
Run
Journal
Registry Snapshot
Hook Binding
Policy
Approval / Decision
Invocation
Receipt
可信预算、计数和时间
Run outcome（通用终态事实）
```

Kernel **不得理解或拥有**：

```text
Route
Feishu群到Agent映射
Context文件规则
Memory / MEMORY.md
Coding任务
开发进度
工作流
Agent角色
多Agent编排
task / job / checkpoint
```

Kernel **不读取**：

```text
bindings/feishu.json
AGENTS.md
SOUL.md
USER.md
MEMORY.md
项目Git仓库
```

这些都属于外部 Harness。

---

## 3. Harness 默认解耦

目标 Harness 组件：

```text
Agent Loop Harness        ← 本 V1 阶段实现 Bootstrap
Route Harness             ← Self-hosting 阶段，由持久 Agent 自主开发
Context Harness           ← 未实现
Memory Harness            ← 未实现
Coding Harness            ← 已存在，作为共享代码工具
Research Harness          ← 未实现
Deployment Harness        ← 未实现
Reviewer Harness          ← 未实现
Agent Communication Harness
```

默认规则：

* Harness 不直接依赖另一个 Harness 的内部数据库；
* Harness 不依赖完整 Kernel 内部领域模型；
* 通过**窄请求、响应、引用、digest 和 Receipt** 组合；
* 一个 Harness 故障不应拖垮无关能力；
* 不建设包办所有功能的 Personal Agent Harness；
* 不建设中央 Development Harness；
* 不预先建设 Development Orchestrator；
* 多 Agent 团队由**独立 Agent 在真实协作中逐渐形成**。

---

## 4. Hook 的调用方向（冻结）

> Hook 不是必须由 Kernel 调用。
> **拥有某个流程步骤的组件，负责调用该步骤的 Hook。**

```text
Agent Loop Harness
→ 调用 Route Harness（未来）
→ 调用 Context Harness（未来）
→ 调用 run.outcome.resolve.v0（本阶段，外部对外部）

Context Harness（未来）
→ 可以调用 Memory Harness（未来）

Agent
→ 可以调用 Coding、Research、Memory、Deployment 等 Harness
```

只有 Kernel 必须可信执行的治理边界（例如 Run Budget）才由 Kernel 调用或验证对应 Hook。

**关键边界**：`run.outcome.resolve.v0` 是**外部对外部**的契约名称。
Kernel 只暴露通用 Run outcome 事实供读取，并接受经授权的同 Session 新 Run 请求。
**Kernel 永远不调用 `run.outcome.resolve.v0`，也不持有 ContinuationPolicy 等产品模型。**

---

## 5. Run Outcome Hook（冻结）

统一使用**一个**通用 Hook：

```text
run.outcome.resolve.v0
```

**不要**分别增加 `run.yield.v0` / `run.complete.v0` / `run.failed.v0`。

### 输入（只允许通用运行事实）

```text
agent_id
session_id
run_id
outcome
outcome_reason
run_budget
budget_exhaustion_reason
automatic_run_count_since_user_input
last_model_disposition
```

`outcome` 取值：

```text
completed
yielded
waiting_user
failed
cancelled
```

禁止输入：

```text
development_task
Router状态
current_progress
next_step
MEMORY内容
Git业务状态
产品类型
```

### 输出

```text
continue_same_session
reply_and_wait
stop
retry_after
```

可附带：

```text
delay_ms
policy_reason_code
policy_version
```

不得返回自然语言产品计划。

> 实现注记：本 V1 阶段，`run.outcome.resolve.v0` 的策略逻辑**内联在
> `tools/agent-loop-harness` 中**（V0 默认策略，见 §7）。契约名称作为外部约定存在；
> Kernel 中仅作为通用 `HookBinding` 数据值登记用于命名/审计，Kernel 不发起该调用。
> 未来该策略若拆为独立 Outcome Policy Harness 进程，本契约名不变。

---

## 6. 调用链（冻结目标链路）

```text
1. Kernel 完成一次有界 Run
2. Kernel 保存通用 Run outcome（通过 Journal 终态事件与 run status 表达）
3. Agent Loop Harness 接收/观察到 outcome
4. Agent Loop Harness 应用 run.outcome.resolve.v0（V0：内联策略）
5. 若为 continue_same_session：
   Agent Loop Harness 请求 Kernel 在同一 Session 创建下一 Run
   （POST /v1/session-continuation）
6. Kernel 验证身份、Session、Registry、Budget 并记录新的 Run
7. 用户不需要发送“继续”
```

关键边界：

* Kernel 不决定是否还有工作未完成；
* Kernel 不根据开发、写作或研究类型续跑；
* Outcome Policy 逻辑不管理代码、Memory 或任务进度；
* Agent Loop Harness 不保存强制的 task/progress schema；
* 下一 Run 依赖 Session 上下文、正常 compaction、工具结果和外部真实状态自行继续。

---

## 7. Bootstrap 阶段：同 Session 自动续跑 V0

### 7.1 存在启动循环

```text
飞书Agent要自主完成长开发
→ 先需要同 Session 自动续跑
同 Session 自动续跑尚未存在
→ 第一版无法依赖它自身完成
```

因此允许当前实施 Agent 完成一次性 Bootstrap。

### 7.2 Bootstrap 范围（只做这些）

```text
1. 最小外部 Agent Loop Harness V0（独立进程）
2. Kernel 最小 /v1/session-continuation seam
3. 同 Session 自动续跑真实 E2E
4. 本文档
```

### 7.3 Bootstrap 边界

* Agent Loop Harness **必须是独立外部进程**；
* 策略和循环**不放进 Kernel**；
* Kernel **不增加** task、progress、checkpoint、Development Job、产品状态机；
* Harness 与 Kernel 之间**只通过 HTTP 窄契约**交互（`/v1/events` 读取、
  `/v1/session-continuation` 请求），不直连 Kernel 数据库、不依赖 Kernel 内部类型；
* 不实现 Route、Context、Memory；
* 不建设中央 Development Harness 或 Orchestrator。

### 7.4 V0 默认策略（run.outcome.resolve.v0，外部）

```text
outcome = yielded
且 last_model_disposition != waiting_user
且 automatic_run_count_since_user_input < max_automatic_runs_since_user_input
且 总续跑墙钟时间 < max_total_wall_time_since_user_input
且 连续失败数 < max_consecutive_failures
→ continue_same_session
```

其他结果：

```text
completed  → reply_and_wait
waiting_user → reply_and_wait
failed     → stop
cancelled  → stop
```

外部总上限（Harness 本地配置，**不进入 Kernel 产品模型**）：

```text
max_automatic_runs_since_user_input
max_total_wall_time_since_user_input
max_consecutive_failures
```

### 7.5 Kernel 改动条件

默认**不修改 Kernel**。只有源码证明缺少以下通用 seam 时，才允许一个最小 Kernel 改动：

```text
读取通用 Run outcome          ← 已存在（/v1/events cursor API + run status）
允许经授权外部 Agent Loop 请求同 Session 新 Run  ← 新增 seam
验证 Principal / Session / Registry / Budget    ← 已存在
记录新 Run 和治理事实            ← 已存在
```

禁止增加：

```text
ContinuationPolicy 产品模型
Development 状态机
task_id / progress / checkpoint
Router 特判
Memory 特判
```

### 7.6 续跑窄契约（审查修复后冻结）

`POST /v1/session-continuation` 请求体只允许：

```json
{
  "trigger_run_id": "run_xxx",
  "expected_session_id": "session_xxx",
  "idempotency_key": "continuation:run_xxx"
}
```

* `expected_session_id` 可选，只用于一致性校验；
* **idempotency_key 严格验证**：必须等于 `"continuation:" + trigger_run_id`，
  不匹配直接拒绝（400），不创建任何事件 / ledger / worker job —— 不同 trigger
  不可能复用同一个合法 key；
* 外部 Harness **不是身份、路由和会话事实的来源** —— Kernel 根据
  `trigger_run_id` 从自身记录加载 prior Run → session_id → agent_id →
  principal → channel → conversation target → Registry Snapshot；
* **下一 Run 完整继承 trigger Run 的冻结事实（High 1）**：使用
  `trigger.agent_id`、`trigger.registry_snapshot_id`（加载该固定 Registry
  Snapshot）、`trigger.principal` / 已冻结 grants。不得读取当前
  KernelConfig.agent_id、不得调用 current_registry_snapshot_id()、不得按
  feishu conversation_key 前缀推导 chat_type、不得按当前 Snapshot 重新扩充
  grants。续跑过程中更换 Agent / 工具版本 / 权限不可能发生；
* 续跑**不伪造用户消息**：不创建 `IngressAccepted` /
  `RuntimeEventPayload::UserMessage` / "继续" 文本。Kernel 只记录通用治理事件
  `SessionContinuationRequested`（request_id / trigger_run_id / session_id /
  requesting_principal / idempotency_key），并调度下一 Run 复用同一 Session
  上下文（前文、compaction、工具结果）自行继续；
* **ledger 是唯一可信事实（High 4）**：`next_run_id` 在接受事务中**预分配**，
  与 `SessionContinuationRequested` 事件、ledger 行、`schedule_continuation`
  worker job 在同一事务内全部成功或全部回滚（普通 INSERT，不用未经检查的
  INSERT OR IGNORE）。worker 使用**预分配**的 next_run_id 幂等创建 Run：
  Run 不存在 → 创建一次；相同 Run 已存在且事实一致 → 视为成功；事实冲突 →
  fail closed。因此同一 trigger（相同 key / 并发 / Harness state 丢失 /
  重启后重新观察 / worker 崩溃重试）都收敛到同一个 next_run_id；重复请求
  立即返回 `{duplicate: true, trigger_run_id, next_run_id}`，无需等待 worker；
* **任何面向用户的"请发送继续"都不存在（High 3）**：预算 yield、follow-up
  LLM 失败、重复工具调用停止，都只记录结构化事实或发送中性说明（如
  "本次执行因模型调用失败而停止"），绝不指导用户发送"继续"或暗示"下一 Run
  接着处理"。只有正常完成、明确等待用户、达到外部上限、最终失败才向用户
  发送消息。

### 7.7 Bootstrap 停止条件

```text
用户只发送一次请求
→ Agent 连续运行至少 3 个有界 Run
→ session_id 保持不变
→ 用户无需发送“继续”
→ 最终正常完成或明确等待用户
```

达到后**立即停止扩展**，不继续 Route Harness。

---

## 8. Self-hosting 阶段（Bootstrap 之后）

Bootstrap 完成后进入 Self-hosting 阶段。

> “外部 Harness”指**运行和责任边界位于 Kernel 外部**，
> 不限制其第一版必须通过飞书开发。

### 8.1 Self-hosting 第一项验收任务

```text
从非生产飞书测试群发送一句话
→ 持久 Agent 自主开发 Route Harness V0
→ 跨多个 Run 继续（依赖 Bootstrap 的同 Session 续跑）
→ 调用 Coding Harness 调查、修改和测试
→ 创建 PR
→ 无需用户发送“继续”
```

### 8.2 此后默认

```text
Route / Context / Memory / Coding 等外部 Harness 的开发与演进
→ 优先由飞书中的持久 Agent 发起并完成
```

### 8.3 正确模型（不是中央 Development Harness）

```text
每个 Agent 拥有基础、完备的开发能力
→ 使用共享的 Coding Harness 作为代码工具
→ 未来通过 Agent 间委托自然形成团队
```

Kernel 始终只提供**通用治理事实和最小 Session/Run seam**，
不理解任何 Harness 的产品语义。

---

## 9. 本文档必须明确的 8 点

1. `yield` 是 Run outcome，同时是外部 Run Outcome Hook 的触发场景；
2. 统一使用 `run.outcome.resolve.v0`，不为每种终态建立独立 Hook；
3. Hook 由**拥有对应流程步骤的 Harness 调用**，不默认由 Kernel 调用；
   特别地，`run.outcome.resolve.v0` 由 Agent Loop Harness 调用，**Kernel 不调用**；
4. Kernel 完全不知道 Memory 概念；
5. Harness 可以组合，但默认通过**窄契约解耦**；
6. Kernel 不知道外部能力的产品类别；
7. 当前阶段只实现**同 Session 自动续跑**（Bootstrap）；
8. 每次开发前必须先读取本文档并回答 §1 防跑偏问题。

---

## 10. 开发治理

执行顺序：

```text
边界确认
→ 只读调查
→ 冻结最小实现方案
→ 实施 + Agent 自主实现和测试
→ 创建 PR
→ 独立 Reviewer 只审 Blocker / High
→ 固定 SHA
→ Canary 部署
→ 真实 E2E
→ 达到停止条件后停止
```

任何修改若与本文冲突，必须标记：

```text
ARCHITECTURE_CHANGE_REQUIRED=true
```

不得把架构变更伪装成普通修复。

---

## 11. 本 V1 阶段禁止事项

* 不继续开发 Router；
* 不实现 Route / Context / Memory Harness；
* 不建设 Development Harness；
* 不建设 Orchestrator；
* 不引入 task / job / progress / checkpoint；
* 不重构 Kernel；
* 不清理全部历史产品语义；
* 不自动合并；
* 不连接正式飞书群。

---

## 相关文档

- [外部编排边界](./external-orchestration-boundary.md) — Kernel 治理 vs 外部工作方法
- [Extension Hook and External Harness Boundary v0](./extension-hook-and-external-harness-boundary-v0.md)
- [Kernel Negative Constitution](./KERNEL_NEGATIVE_CONSTITUTION.md)
- [Kernel Primitive Calculus](./kernel-primitive-calculus.md)

*End of document.*
