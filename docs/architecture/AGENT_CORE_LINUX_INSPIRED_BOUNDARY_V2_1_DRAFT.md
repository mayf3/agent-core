# AGENT_CORE_LINUX_INSPIRED_BOUNDARY_V2_1_DRAFT

状态：DRAFT
用途：Agent Core V2 架构边界讨论稿
主要参照：Linux Kernel / Userspace 分层思想，但不追求与 Linux 一一对应。

---

## 1. 北极星

Agent Core 的长期目标不是让 Kernel 成为 Agent 的管理者，而是让一个持久 Agent 能够逐渐自治地完成：

需求理解 → 调查 → 执行 → 验证 → 复盘 → 学习 → 修改自身外部能力 → 下一次做得更好。

人类可以在早期承担部分授权和高风险决策。

但长期方向不是增加更多：

“请人在飞书点一下同意。”

而是逐步将决策能力交给 Agent、Policy Harness、Authority 等外部自治组件。

Kernel 不应该因为自治程度变化而不断修改。

---

## 2. Linux 是参照，不是模板

长期使用 Linux 检查 Agent Core 的边界：

* 哪些是稳定机制？
* 哪些是 userspace policy？
* 哪些是 application runtime？
* 哪些只是当前产品偶然存在的概念？

但禁止：

“Linux Kernel 有 X，所以 Agent Core Kernel 也必须有 X。”

Agent Core 与 Linux 的核心差异包括：

* Agent 的核心计算发生在外部模型服务；
* Harness 是分布式服务；
* 调用可能出现 outcome unknown；
* Agent 需要长期 History → Reflection → Behavior Change；
* 权限治理最终希望能够由 Agent 自治。

因此只能借 Linux 的边界思想，不机械复制。

---

## 3. 三个长期角色

### Agent Runtime

负责：

Session
Run
LLM loop
Context
ToolCatalog
Continuation
Agent scheduling
Final reply
Reflection
行为决策

人话：

> Agent Runtime 负责“这个 Agent 怎么活着、怎么思考、下一步想干什么”。

### External Harness / Capability Provider

负责：

真实执行
workspace
filesystem
shell
git
build/test
network environment
sandbox
具体业务能力
详细 execution evidence

人话：

> Harness 负责“把事情真的做出来”。

### Kernel

长期只保留它作为公共调用边界不可替代的机制。

人话：

> Kernel 不经营 Agent，也不理解业务；它只保证外部调用能够被正确绑定、转发和追踪。

---

## 4. Approval 从 Kernel 删除

Kernel 不拥有：

Approval workflow
Approval state
用户确认 UI
“为什么批准”的逻辑

长期权限自治采用外部 Authority。

早期：

Human → Authority → Capability Handle

中期：

Policy Harness + Human fallback → Authority → Capability Handle

长期：

Agent / Policy Agent → Authority → Capability Handle

Kernel 始终只机械验证当前 handle 是否有效。

因此自治程度提高不需要改变 Kernel。

---

## 5. Policy 原则

Kernel 不理解：

production
test
public network
private network
real asset
database
Route
Coding
Memory
危险 URL
安全命令

这些语义属于 Harness / Infra / Policy Authority。

Kernel 不负责判断：

“这个行为危险不危险。”

Kernel 最多机械判断：

Actor A 当前是否持有 Handle H？

H 是否有效？

H 是否已撤销？

如果需要更复杂 Policy，由外部 Authority 在 handle acquisition 阶段决定。

---

## 6. fd / Capability Handle 是 V2 核心方向

长期参考 Linux fd 思想。

不是字面上的“一切都是文件”。

真正原则是：

> 尽量让外部能力表现成可以打开、持有、使用、撤销、关闭的不透明句柄。

例如：

```text
H17 → provider=P3 / capability=C8 / version=V2
```

Agent 不需要每次调用都重新经历：

Registry → Profile → Policy → Grant → ToolCatalog filtering

而是先取得能力：

```text
open capability
→ H17
```

之后：

```text
invoke(H17, args)
```

Kernel 不需要理解 H17 的产品名称。

---

## 7. Registry 不等于 Kernel Registry

系统仍然需要知道：

“现在世界上有哪些 Harness / Capability。”

但这个目录属于外部 Capability Directory / Broker。

它可以知道：

workspace.exec
history.search
route.resolve
...

Kernel 不应该依赖这些名字进行权限判断。

Kernel 最多维护执行所需的最小 handle binding：

```text
H17
→ provider endpoint/reference
→ capability reference
→ version/generation
→ holder
→ lifetime
```

---

## 8. Registry Snapshot 的一致性需求保留，但实现可以删除

旧 Registry Snapshot 的真实需求是：

> 一段正在进行的工作不要突然因为 Registry 更新而看到完全不同的一组能力。

这个需求不能丢。

fd-style model 通过 handle acquisition 自然解决：

H17 在取得时已经绑定 provider/version。

只要 H17 仍然有效，后续 invoke(H17) 继续使用原来的绑定。

因此：

> 保留 snapshot consistency，
> 不一定保留 Registry Snapshot 这个 Kernel 产品模型。

---

## 9. ToolCatalog 外移到 Agent Runtime

模型需要看到：

```text
workspace.exec
history.search
system.status
```

以及参数 schema。

这是 LLM Context / Agent Runtime 的职责。

Agent Runtime 根据当前持有的 handles 和外部 capability metadata 生成 ToolCatalog。

例如：

```text
模型看到：workspace.exec
Runtime 映射：workspace.exec → H17
Kernel 收到：invoke(H17, args)
```

Kernel 不参与：

Tool 名字选择
Tool 描述
JSON Schema
Prompt 构建
ToolCatalog filtering

---

## 10. Session / Run 保留概念，但倾向从 Kernel 外移

Session 人话：

> 一段持续的 Agent 经历。

Run 人话：

> 其中一次有限的 Agent Loop。

它们对 Agent 很重要，因此不能简单删除。

但是：

> 对 Agent 重要 ≠ 必须 Kernel-owned。

长期目标倾向：

```text
Agent Runtime owns:
Session
Run
Continuation
LLM loop
```

当 Agent 调用 Kernel 时，可以携带：

```text
session_id
run_id
```

作为 correlation metadata（关联标签）。

Kernel 可以记录它，但不一定负责：

create_run
continue_run
finish_run

现有 Same-Session Continuation 是已验证成功的阶段性实现。

是否迁移到 Agent Runtime，需要单独迁移设计，不在本稿直接执行。

---

## 11. Agent Scheduling 倾向外移

Linux CPU scheduler 必须在 Kernel，是因为 CPU 是 Linux Kernel 管理的资源。

Agent Core 并不直接拥有：

模型 GPU
Harness CPU
Deployment runtime

所以：

```text
下一轮什么时候开始
Agent 是否继续思考
等哪个 Harness 完成再唤醒
定时什么时候执行
```

更像 Agent Runtime / Scheduler Service 的职责。

长期倾向：

```text
Agent Runtime owns logical scheduling
External Scheduler owns durable timers/wakeups
```

Kernel 不拥有 Agent 业务调度策略。

---

## 12. Invocation（调用实例）是 Kernel 的强候选核心概念

Invocation 人话：

> 某个 Actor 真正对一个 Handle 发起了一次调用。

例如：

```text
Invocation I88

Actor=main
Handle=H17
arguments_digest=...
started_at=...
```

Kernel 天然位于所有受管调用的交界点，因此非常适合产生唯一 Invocation ID 并关联后续结果。

这是目前最有理由留在 Kernel 的概念之一。

---

## 13. Result（调用结果）属于 Invocation

普通调用不需要独立 Receipt 系统。

长期倾向：

```text
Invocation
├ status
├ result
└ evidence_ref
```

例如：

```text
I88
status=succeeded
result=exit_code:0
evidence_ref=E91
```

---

## 14. Receipt（执行方回执）不再作为 Kernel 一级概念

Receipt 人话：

> 外部执行方说“这一次我确实执行过”。

它在分布式、不可重复的操作中可能很有价值，例如发生：

请求发送
→ Provider 已执行
→ 网络断开
→ 调用方没收到结果

这时 Provider Receipt 可以帮助恢复真实 outcome。

但它属于具体 Provider 的 Evidence / protocol。

不要求所有普通 read/build/test 调用都产生重量级 Receipt。

因此：

> Receipt 可以存在，但默认不再是 Kernel 一级对象。

---

## 15. Event（事件）和 Hook（钩子）必须区分

Event：

> “刚刚发生了什么。”

用于：

History
Observability
Reflection

例如：

```text
InvocationStarted
InvocationCompleted
InvocationFailed
```

Hook：

> “事情经过这里时，让另一个模块有机会插手。”

用于：

Policy interception
validation
synchronous extension point

History 使用 Event，不应该使用同步 Hook。

否则 History Service 会成为执行链路的同步依赖。

---

## 16. Kernel Journal 倾向收敛成 Event Source

长期借鉴：

Kernel audit events → userspace audit daemon

Kernel 负责产生可信的最小 Event：

```text
event_id
actor
invocation_id
correlation metadata
status
evidence digest/ref
timestamp/order
```

外部 History Harness / Daemon 负责：

持久化
索引
全文查询
retention
compression
analytics
长期存储

Kernel 不负责完整 History 产品。

为了避免 History Daemon 临时不可用导致记录直接丢失，Kernel 可以拥有一个非常小的可靠 spool / WAL。

它的职责只是：

> event 尚未可靠交付时暂存。

而不是发展成完整 Journal 数据库。

---

## 17. History ≠ Context

History：

> 过去真实发生过什么。

应尽量完整、长期保存。

Context：

> 当前这一轮模型需要看到什么。

允许：

选择
压缩
总结
丢弃无关信息

不能因为 Context compaction 删除 History。

长期自进化链：

```text
Kernel Events
+
Harness Evidence
        ↓
History Service
        ↓
Reflection / Memory Agent
        ↓
经验
        ↓
改变 Agent / Skill / Harness
```

Kernel 是事实源之一，不是学习者。

---

## 18. Generic Execution Runtime

Agent owns coding behavior。

External Execution Runtime owns execution environment。

长期只保留一个底层 Execution Authority：

```text
workspace
filesystem
shell
git
build/test
process
sandbox
```

不得长期同时维护：

Coding Harness execution stack
+
Generic Execution Harness execution stack

具体如何从当前实现收敛，需要后续迁移决策。

---

## 19. Kernel 不理解 workspace.exec 内部语义

Kernel 不解析：

curl
git
cargo
python
npm

Kernel 不判断：

URL 是不是公网
是不是生产
是不是“真实资产”

Execution Harness / Infra 决定真实环境边界。

Kernel 只知道：

```text
Actor A
invoked Handle H
Invocation I
Result R
Evidence E
```

---

## 20. Execution 必须可观察，但 Kernel 不需要全知

Execution Harness 保存丰富 evidence：

```text
command
cwd
stdout/stderr
file diff
process tree
network observations
test logs
```

Kernel 保存：

```text
Invocation
Result
EvidenceRef
EvidenceDigest
```

Reflection Agent 可以：

```text
Kernel timeline
→ 找到相关 Invocation
→ 跟随 EvidenceRef
→ 查看 Harness trace
→ 总结经验
```

目标是：

> 可追溯，而不是 Kernel 全知。

---

## 21. 当前 Kernel 一级概念候选

目前强候选：

```text
Actor / Credential Reference
Capability Handle
Invocation
Invocation Result
Event Emission
最小可靠 Event Spool
```

仍需继续挑战：

```text
Capability Handle 是否必须 Kernel 维护
Actor credential 到底保存多少
Event spool 最小需要多少
```

---

## 22. 当前倾向外移

```text
Approval
Approval State
Policy reasoning
Capability Directory / Registry
ToolCatalog
Session lifecycle
Run lifecycle
Agent Loop
Same-Session Continuation 最终 ownership
Agent Scheduling
Final Reply
Context
Memory
Reflection
Durable History Storage
Receipt 作为统一一级模型
```

“倾向外移”不代表立即重构。

需要逐项证明迁移路径。

---

## 23. 当前 Generic Execution PoC

真实飞书持久 Agent main 已证明：

在获得通用 execution tools 后，可以直接：

filesystem
shell
git
build/test
local HTTP probe

并且：

```text
external.coding_task_submit calls=0
```

因此：

```text
GENERIC_EXECUTION_POC=PASS
```

但当前实现借用了旧：

```text
external.coding_workspace_*
```

Kernel allowlist。

这只是 PoC 兼容方式。

不得作为最终 V2 capability identity 冻结。

---

## 24. 当前最重要的开放问题

下一阶段继续讨论而不是立即实现：

### A. Capability Handle acquisition

谁可以发 Handle？

Handle 是 Kernel table entry、签名 token，还是两者结合？

如何 revoke？

如何 expire？

如何继承？

---

### B. Authority

前期：

Human-in-the-loop

中期：

Policy Harness + Human fallback

长期：

Agent-governed Authority

Kernel 不应该因为 Authority 从人变 Agent 而变化。

---

### C. Session / Run migration

什么时候可以将现有 Agent Loop / Same-Session Continuation 从 Kernel 搬到 Agent Runtime？

需要哪些兼容和 Canary 证据？

---

### D. Scheduling

Agent logical scheduling 和 durable wakeup 应分别属于谁？

是否需要独立 Scheduler Harness？

还是 Agent Runtime + 普通 timer 能够解决？

---

### E. Kernel Event Reliability

Kernel 到 History Service 的 Event：

至少一次？
至多一次？
允许重复、由 event_id 去重？
History Service 不可用时 spool 多大？

这是 History 可靠性的核心问题。

---

## 25. 当前一句话边界

> Agent 决定并学习。

> Harness 执行并留下丰富证据。

> Agent Runtime 管 Agent 的生命、Run、Context 和工具视图。

> Kernel 只维护稳定的不透明能力调用边界，并产生可信事件。

> History Service 保存长期历史。

长期目标不是增加中央控制，而是：

> 让 Agent 在可靠历史和稳定边界上逐渐获得更强自治能力。
