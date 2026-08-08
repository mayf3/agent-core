# Agent Runtime V0 Semantics

> 状态：**SEMANTICS BASELINE V0**
>
> 本文冻结的是术语与语义讨论基线，不代表 V2.1 ownership 已经完成迁移，也不取代当前 V1 implementation baseline。
> 本文不是实现设计、crate 设计或 migration plan。

## 0. 这份文档回答什么

> Agent Runtime 最基本在管理什么？

一句话：

> 一段持续的事情（Session）、为它的一次有边界开工（Run）、以及开工时模型桌面上实际摆着的材料（Context）。

## 1. 三个主概念

### Session —— 一段持续经历的 identity / 锚点

- 用于把多次 Run 关联为「同一段持续事情」；
- **不是**完整 History database；
- **不要求** Session 自己保存所有历史；
- 长期持久化方式待后续讨论。

### Run —— 一次有边界的开工

- Agent 为这段事情进行的一次开工，有边界（时间 / 轮次 / 预算等语义保留）；
- **明确**：当前 `runs` 表及其 snapshot / grants / budget 等实现，不因此自动冻结为长期 Runtime V0 设计；
- Run 的最小长期实现（字段、持久化、与 journal 的关系）待后续讨论。

### Context —— 当前一次模型思考的桌面

- 每轮产生的工作视图；
- 可来自 Session / history / system / memory / tools / 当前环境；
- **不等于** Session；
- **不等于** History；
- **不作为**长期持久的一级 identity。

## 2. 两个轻量术语

### Turn

> 一次 assistant/model response，以及由这次 response 产生的工具调用和结果。

- 只是 Run 内部的自然边界；
- 可用于 event / observability / counter；
- V0 不要求 turn_id，不要求 turn table，不升级为一级持久对象。

### Invocation

> Agent 真正伸手调用一次外部能力。

- 属于 Kernel，不属于 Runtime；
- 一个 Run 可以包含多个 Invocation。

## 3. 极简图

```text
Session                     = 这段持续的事情
  ├─ Run 1                  = 这次开工
  │    ├─ Turn              = 想一轮
  │    │    └─ Invocation → Kernel → Result   = 真正出去做一下
  │    ├─ Turn
  │    └─ Reply / Yield / Failure
  │
  └─ Run 2
       ...

Context = 这一轮桌面材料（当前一次模型思考时实际摆着的材料）
```

## 4. Run 与 Pi 的关系

- Pi 也有 run 语义和 runId，但 Run 很轻，本质接近一次 agent-loop execution；
- Pi 的长期存档主要由 Session entries 承担；
- Agent Core 当前 Run 更重，是当前治理 / 恢复实现的事实；
- 这只能证明 Agent Core 需要继续研究 durable Run identity，不能证明当前重型 Run 实现必须永久保留。

## 5. 非目标（本文件不解决）

以下全部留待后续讨论，不在此展开：

Trigger / Scheduler / Approval-Authority / Handle acquisition / Memory 架构 / Reflection / History storage ownership / Runtime crate / Runtime-Kernel 进程拆分 / migration plan / Run 最终字段 / Session 最终持久化方式。
