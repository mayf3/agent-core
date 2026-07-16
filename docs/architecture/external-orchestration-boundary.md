# Agent Core 外部编排边界决策

> 状态：架构强约束
> 目的：防止计划、路由、多 Agent、验收和修复策略逐步进入 Kernel。

## 决策

```text
Kernel 负责：
谁、哪次运行、是否允许、执行了什么、结果是什么、启用了哪个版本。

External Harness 负责：
想做什么、怎么做、让谁做、怎么验收、失败后怎么办。
```

## Kernel 可以持有

- Subject / Principal 身份；
- Scope 隔离；
- Run 生命周期；
- append-only Event（Journal 事实）；
- 固定 Snapshot；
- Intent；
- Decision；
- Invocation；
- Receipt；
- 当前激活版本；
- 外部制品的不透明引用和摘要（opaque_ref / digest）。

## Kernel 不得理解

- 自然语言需求；
- 开发计划（DevelopmentPlan）；
- 验收计划（AcceptancePlan）；
- 任务拆解；
- 角色分工（AgentRole）；
- 多 Agent 协作（MultiAgent）；
- 模型选择（ModelSelectionStrategy）；
- Prompt 修复策略；
- 产品业务验收；
- 页面设计；
- 数据聚合策略；
- Cron 业务规则（SchedulerBusinessRule）；
- 记忆抽取（MemoryStrategy）；
- 上下文压缩（CompressionStrategy）；
- 修复流程（Workflow / RepairManager）。

## 外部职责

### Router / Planner Harness

- 理解自然语言；
- 发现可用 Kernel 接口；
- 选择组件形态；
- 制定开发和验收方案；
- 计算最小权限建议；
- 返回声明式 Proposal。

### Multi-Run Orchestrator

- 选择不同 Workspace；
- 加载不同上下文和 AGENTS.md；
- 创建多个普通 Run；
- 安排开发、审计、部署顺序；
- 根据结果继续、返工或停止。

### Coding Harness

- 文件读写；
- 模型调用；
- 编译；
- 测试；
- 产出候选 Artifact。

### Acceptance Harness

- 执行业务验收；
- 输出证据；
- 绑定候选 Artifact digest。

### Deployment Harness

- 安装、启动、停止；
- 健康检查；
- 升级、禁用、回滚；
- 输出强类型 Deployment Receipt。

### Observer / Repair Harness

- 消费事件；
- 检测故障；
- 产生 Repair Request；
- 调用外部编排重新开发和验收。

## 关于计划制品

默认不要求 Kernel 保存计划内容。

若审计或批准确实需要固定某份方案，Kernel 最多记录：

```text
plan_ref
plan_digest
```

它只用于防止外部制品被偷换，不表示 Kernel 理解计划。

## 关于验收

具体产品验收不进入 Kernel。

Kernel 只验证：

```text
验收由受信任 Harness 执行
验收结果绑定正确候选
验收证据摘要完整
```

例如 Token Dashboard 的 1/7/30 日窗口、Token 聚合、延迟和页面展示，都属于外部验收包。

## 关于 Catalog

Kernel 只发布自身稳定接口和权限边界。

接口的发现、组合、示例、模板、SDK 和测试工具属于外部开发系统。

## 关于多 Agent

多 Agent 不是 Kernel 概念。

```text
Multi-Agent
=
外部编排
+ 多个普通 Run
+ 不同 Workspace / Context / Grants
+ 输入输出引用
```

## 防膨胀规则

任何新概念进入 Kernel 前必须证明：

1. 外置会破坏不可绕过安全边界；
2. Kernel 必须亲自验证；
3. 语义长期稳定；
4. 无法由已有治理原语组合；
5. 不是为了当前实现方便。

否则必须外置。

## 禁止新增的 Kernel 产品概念

下列名词可以存在于外部 Harness 的 API、制品和文档中，但不得成为 Kernel 一等对象：

```text
Plan
DevelopmentPlan
AcceptancePlan
Planner
AgentRole
MultiAgent
Workflow
RepairManager
Dashboard
MemoryStrategy
CompressionStrategy
SchedulerBusinessRule
ModelSelectionStrategy
```

## 最终结论

> Kernel 是治理边界，不是工作方法。
> 计划、角色、协作、验收和修复均属于外部 Harness。

## 相关文档

- [Kernel Primitive Calculus](./kernel-primitive-calculus.md) — §16–§22 详细阐述边界判定规则
- [Primitive Screening Matrix](./primitive-screening-matrix.md) — 逐概念筛查证据表
- [Extension Hook and External Harness Boundary](./extension-hook-and-external-harness-boundary-v0.md)
