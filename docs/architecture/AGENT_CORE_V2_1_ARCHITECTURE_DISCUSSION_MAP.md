AGENT_CORE_V2_1_ARCHITECTURE_DISCUSSION_MAP

状态：WORKING DRAFT用途：记录当前阶段已经形成的架构共识；控制概念数量；为后续逐项讨论和仓库迁移提供导航。注意：本文不是新的冻结边界，不代表所有名词都已经证明有必要存在。

## 当前权威关系（Documentation Governance V0）

```text
Current implementation/governance baseline:
    AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1.md（V1 冻结边界）+ 当前代码

Architecture evolution discussion:
    本文档（V2.1 Discussion Map，WORKING DRAFT，尚未取代 V1）

Conflict rule:
    在 V2.1 尚未冻结前，讨论可以挑战 V1 ownership，
    但实际开发不得把讨论稿自动当成已生效架构。
```

已知但尚未裁决的 ownership 冲突（只列名字与指向，不在本文解决）：

* Session
* Run
* Agent Runtime
* Approval
* Registry / ToolCatalog
* Context

Runtime 基础语义见 [AGENT_RUNTIME_V0_SEMANTICS.md](./AGENT_RUNTIME_V0_SEMANTICS.md)。

Runtime ↔ Kernel 调用边界见 [RUNTIME_KERNEL_INVOCATION_BOUNDARY_V0.md](./RUNTIME_KERNEL_INVOCATION_BOUNDARY_V0.md)。

0. 为什么需要这份文档

当前讨论已经从「Kernel 里哪些东西应该搬出去」进入下一阶段：

Kernel 外面的世界应该怎样组织？

这一阶段最大的风险不是缺少抽象，而是抽象和名词增长过快，导致：

同一个问题被多个概念重复表达；

架构图越来越完整，但没人能一次记住；

还没用真实代码证明必要性，就提前冻结新对象；

为了讨论一个局部问题，需要先加载整套架构词汇。

因此接下来采用一个新原则：

先证明问题，再引入概念。先保持少数稳定心智模型，再按需要逐层展开。

1. 当前最小心智模型：先只记三个盒子

现阶段讨论 Agent Core，只要求先记住三个核心概念。

1.1 Agent Runtime

回答：

Agent 怎么活着、怎么跑、什么时候继续、这一轮怎么看世界？

它当前强候选拥有：

Session / Run 的生命周期

LLM loop

Trigger 接入

Continuation

Context assembly

Tool view

Final reply

与 Agent 行为直接相关的运行机制

Runtime 不是安全边界。

Runtime 可以快速演进，未来甚至可以让 Agent 在可验证范围内修改部分 Runtime extension / policy。

现代 Agent 系统（例如 Pi、Claude Code、Codex、OpenCode 一类系统）更适合作为 Runtime 的主要参照对象。

Linux 对 Runtime 的主要价值，不是要求一一映射组件，而是继续提供以下检查方式：

mechanism 和 policy 是否混在一起；

稳定运行机制与快速变化策略能否拆开；

一个概念是否真的需要成为全局基础设施；

能否通过窄接口让上层快速变化，而不破坏底层边界。

1.2 Kernel

回答：

Agent 已经决定要调用一个外部能力后，这次调用是否正确绑定到了它当前真正持有的能力，以及调用实际发生了什么？

长期候选只保留：

Actor / Credential Reference

Capability Reference（fd-style Handle 仅为其候选实现，见 RUNTIME_KERNEL_INVOCATION_BOUNDARY_V0）

Invocation

Invocation Result

Event emission

最小可靠 Event spool / WAL

Kernel 不负责：

Agent 为什么这么做；

下一步应该做什么；

Context 应该放什么；

是否应该 Reflection；

是否应该继续一个 Run；

业务动作危险不危险；

飞书、Coding、Memory、OKR 等产品语义。

一句话：

Runtime 管 Agent 的运行；Kernel 管不可绕过的能力调用边界。

1.3 External World

现阶段先不要强迫自己记住一堆子类型。

凡是不属于 Runtime 或 Kernel 的外部组件，先统一放在：

External World

里面可以暂时包括：

OpenClaw

filesystem / shell / git / build / test

History

Memory storage/search

Workflow

OKR

Forum

Feishu

Deployment

Authority

Capability Directory

Scheduler

Search / Browser

其它 Agent 或服务

只有当一个具体设计问题确实需要区分这些角色时，再继续展开。

2. OpenClaw 的当前定位

这里的“龙虾”统一指 OpenClaw 软件。

当前不把 OpenClaw 强行压进「Harness」或「Runtime」其中一个概念。

更合适的暂定理解是：

OpenClaw 是当前 Agent Host / Distribution。

它可能同时承载：

Feishu 等 channel / gateway

Agent 配置

plugin / skill

runtime integration

session / cron 等现有能力

未来 Agent Core 不应该为了理论纯度重写 OpenClaw 已经做得好的部分。

当前更值得研究的是：

OpenClaw + Pi-like Runtime 能否逐步成为 Agent userspace，而 Agent Core Kernel 收缩成稳定能力边界。

3. Harness 这个词暂时收窄，但不扩大分类树

过去我们把几乎所有 Kernel 外部组件都叫 Harness。

这个做法在 AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1 阶段是有价值的，因为当时最重要的目标是：

只要能在 Kernel 外解决，就不要塞回 Kernel。

这个边界继续有效。

但从现在开始：

Kernel-external ≠ Harness。

为了避免再增加五六个新一级概念，现阶段只做一个最小修正：

Harness 暂定含义

Harness / Capability Provider：把 Agent 已经决定要做的某类真实工作执行出来，并留下 execution evidence 的外部组件。

典型：

filesystem

shell

git

build/test

browser

deployment

某些业务调用

而以下东西暂时不要机械称为 Harness：

Context selection

Compaction

Route decision

Reflection reasoning

Session / Run

LLM loop

它们先归到 Runtime / Agent 行为问题里讨论。

History、Memory、Authority、Scheduler 等是否需要再分独立类别，暂不冻结。

4. Event 与 Hook：目前只保留一个简单判断

Event

Kernel 的标准机制。

回答：

已经发生了什么？

例如：

InvocationStarted

InvocationCompleted

InvocationFailed

HandleOpened

HandleRevoked

Event 面向 History / Observability / Reflection。

Kernel Hook

不是通用插件框架。

只在必须同步保护 Kernel-owned invariant 时考虑，而且数量应非常少。

Runtime Hook / Extension

可以丰富得多。

因为 Runtime 的目标之一就是允许：

Context 策略变化

Compaction 策略变化

Tool presentation 变化

Routing 变化

Reflection trigger 变化

Model strategy 变化

Continuation strategy 变化

因此当前原则是：

Kernel 抵抗扩展；Runtime 欢迎演进。

但 Runtime 的演进不能绕过 Kernel 获取不存在的 capability。

5. 为什么 Runtime 不能只是「第二个 Kernel」

两者的失败语义不同。

Runtime 写坏

可能导致：

Agent 少跑 / 多跑一轮

Context 错误

Tool view 错误

Continuation 失败

Agent 变笨

Session 状态混乱

这些是严重的 Agent correctness 问题。

Kernel 写坏

可能导致：

Actor 使用了不属于自己的 capability

revoked handle 仍然可用

Invocation 绑定错误

受管调用无法可信追踪

这是系统边界被破坏。

因此：

Runtime 追求行为质量和快速演进。

Kernel 追求边界正确性和极低变化率。

6. 当前 Trigger / Run 的最小方向

短期最重要的真实场景仍然是：

飞书给持久 Agent 发一条消息，然后 Agent 真正开始执行。

长期倾向：

Feishu
  ↓
OpenClaw / Connector
  ↓
Trigger
  ↓
Agent Runtime
  ├─ find/create Session
  ├─ create Run
  ├─ LLM loop
  ├─ context/tool view
  └─ continuation/final reply
        ↓
      invoke(handle)
        ↓
      Kernel
        ↓
      External capability

关键原则：

Kernel 不负责“为什么现在应该创建一个 Run”。

Run 是 Agent Runtime 的生命循环概念。

Kernel 可以记录 session_id / run_id 作为 correlation metadata，但不因此拥有 Session / Run lifecycle。

7. 下一阶段只讨论三个问题

为了控制信息熵，接下来不同时展开 Authority、Scheduler、History、Directory、Receipt 等所有问题。

下一阶段只聚焦三个概念。

A. Agent Runtime

目标：

明确 Runtime 到底必须拥有的最小 mechanism。

重点研究：

Pi / Claude Code / Codex / OpenCode 的 runtime loop

Linux 的 mechanism / policy 分离思想

当前仓库真实 Run / Session / continuation 实现

核心问题：

Runtime Engine 最小应该是什么？

B. Trigger / Wakeup

目标：

明确一个持久 Agent 为什么、何时、由谁被唤醒。

先覆盖：

Feishu message

same-session continuation

timer（只讨论语义，不急着引入 Scheduler Service）

外部任务完成后的再次唤醒

核心问题：

Trigger 是什么最小输入？durability 放在哪里？

C. Session / Run

目标：

明确 Agent 的持续经历和有限执行单元到底需要什么语义。

核心问题：

Session / Run 是否都是必要概念？

Turn 是否已经足够？

Run 与 Invocation 的边界是什么？

crash/restart 后需要恢复到什么程度？

Same-Session Continuation 迁移时必须保留哪些已验证性质？

8. 暂停展开的概念

以下概念全部进入 Parking Lot。

只有当上面三个问题真的要求它们存在时，再取出来讨论：

独立 Scheduler Service

Authority 的最终形态

Capability Directory / Broker

State Service 分类

Memory 独立一级架构

History 独立一级架构

Runtime Policy 独立一级架构

Runtime Extension Framework 的正式抽象

Receipt 是否保留

Handle 是 table / token / hybrid

Handle inheritance

多 Runtime

多 Host

Agent-to-Agent routing infrastructure

原则：

不因为“以后可能需要”就提前增加一级概念。

9. 新概念的准入规则

今后任何新一级架构概念，至少满足下面一个条件才允许加入主图：

独立 ownership没有它就无法说明谁负责什么。

独立 failure mode它坏掉的含义和现有组件明显不同。

独立 trust boundary它必须有与其它组件不同的可信边界。

独立 lifecycle它需要独立创建、恢复、升级或销毁。

真实迁移需要当前仓库已经出现一个具体迁移问题，现有概念无法表达。

如果只是：

更方便画图；

Linux / 某产品里有同名东西；

将来可能会需要；

可以让架构显得完整；

则不能成为新的一级概念。

10. 分阶段披露规则

以后架构讨论默认只展示当前问题需要的层级。

Level 0：默认

只说：

Agent Runtime
Kernel
External World

Level 1：讨论 Runtime

才展开：

Runtime
├ Trigger
├ Session / Run
├ LLM loop
├ Context / Tool view
└ Continuation

Level 2：讨论一次外部调用

才展开：

Runtime
  ↓ invoke(handle)
Kernel
  ├ Capability Reference
  ├ Invocation
  ├ Result
  └ Event
External Capability

Level 3：只有具体问题需要才展开

例如：

Authority

Scheduler

Directory

History

Memory

Reflection

Provider evidence

Runtime extension

避免一次把完整系统全部摊开。

11. 当前不变的旧边界

AGENT_CORE_EXTERNAL_HARNESS_BOUNDARY_V1 的核心精神继续有效：

Agent、Harness、Reviewer、Deployment、Infra 等外部层能够解决的问题，不得无证据回流 Kernel。

V2.1/V2.x 不是推翻它，而是开始细化：

Kernel 外部到底怎么组织。

因此：

过去的“External Harness”可以理解成广义 Kernel-external userspace；

新讨论中 Harness 逐步收窄为真实 capability execution provider；

但任何词义变化都不能成为把能力重新塞回 Kernel 的借口。

12. 当前一句话架构

最小版本：

Runtime 让 Agent 活着并运行。

Kernel 保证受管能力调用的边界正确。

其它东西先都留在 Kernel 外，只有证明必要时再分类。

当前阶段的工作目标不是画出最终完整架构，而是：

用最少概念，逐步证明一个持久 Agent 从 Trigger → Run → Action → Continuation 的真实运行边界。
