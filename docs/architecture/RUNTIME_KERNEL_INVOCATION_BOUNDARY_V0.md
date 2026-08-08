# Runtime ↔ Kernel Invocation Boundary V0

> 状态：**SEMANTICS BASELINE V0**
>
> 冻结的是讨论语义基线，不代表当前 V1 implementation 已完成迁移。
> 本文不设计 Handle acquisition / Authority / migration。
>
> **Change note（Red Team Review 修正）**：
> 删除业务 request claim / dedupe；把 fd-style Handle 从既定答案降级为 capability
> reference 的候选实现；强化 Invocation identity 与 durable-state 语义。

## 0. 这份文档回答什么

> Runtime 已经决定「我要做一个真实外部动作」以后，Runtime 和 Kernel 之间到底发生什么？

整篇只有两个核心概念：**Capability Reference** 和 **Invocation**。

## 1. 两个核心概念

### Capability Reference —— 钥匙 / 门禁卡（你凭什么能碰）

表达：Runtime 已经获得了使用某项外部能力的资格。

**它不是最终冻结机制**。本文件只定义语义要求：

> Runtime 调用真实外部能力时，必须携带一个**可信的 capability reference**——
> 也就是"凭什么有资格做这件事"。

它最终可以是：

- table handle；
- signed token；
- credential reference；
- 其它不可伪造引用。

**fd-style Handle 只是当前候选实现之一，不是已经证明的最终架构承诺。**

Kernel 不需要重新理解：

- 这是 shell 还是 git；
- 这是 Coding 还是 Workflow；
- 这是生产还是测试；
- 为什么 Agent 想这么做。

Kernel 只机械检查：

- 这个引用是真的吗；
- 是当前调用者持有的吗；
- 是否过期；
- 是否撤销；
- 是否仍然绑定到有效能力。

（acquisition / Authority 不在本文件范围。）

### Invocation —— 一次真实外部调用的持久、不可变、可信记录

Invocation 不只是"流水号"。人话：

> Kernel 的调用登记簿里的一笔。

Runtime 在调用前产生稳定的 Invocation ID，例如 I88：

```text
submit(I88, capability_ref, args)
```

Kernel 对 I88 只保证四件事：

1. **I88 首次出现**：
   - 验证 caller + capability reference；
   - 原子登记 I88；
   - 冻结这一次调用对应的 capability binding 与请求内容。
2. **再次提交完全相同的 I88**：
   - 返回 I88 当前状态；
   - 不自动重新执行。
3. **I88 已存在但 capability / request 内容不同**：
   - identity conflict。
4. **I89 即使 args 与 I88 完全一样**：
   - 是另一笔新调用；
   - Kernel 不判断它是不是业务重试。

## 2. Kernel 不负责业务幂等

明确否定以下目标：

- Kernel 判断 I88 / I89 是否是同一个业务动作；
- request_key 业务去重；
- exactly-once side effect；
- 自动 retry / reconciliation；
- compensation；
- 根据 args 推断"是不是重试"。

> 两个不同 Invocation 是否在业务上重复，是 Runtime / Provider / 上层协议的问题。

Provider 如果支持 order_id / message key / InvocationId 幂等，那是 **Provider
protocol 的能力**，不是 Kernel 的承诺。

## 3. Invocation 最小状态

```text
ACCEPTED
   ↓
DISPATCHED
   ├─ SUCCEEDED
   ├─ FAILED
   └─ UNKNOWN
```

语义（人话）：

- **ACCEPTED**：Kernel 已可靠登记，但能证明还没有跨过真实执行边界；
- **DISPATCHED**：请求可能已经交给 Provider；
- **SUCCEEDED**：拿到可信成功结果；
- **FAILED**：Provider 明确报告终态失败——不要解释成"外部世界一定没有发生任何变化"；
- **UNKNOWN**：已经可能发出，但 Kernel 无法证明最终结果。

必须强调：

> UNKNOWN 是诚实状态，不得擅自降成 FAILED 或 SUCCEEDED。

一旦到 DISPATCHED / UNKNOWN：

> Kernel 默认不自动重新 dispatch。

是否重试由 Runtime / Provider protocol 决定。

## 4. crash 语义

最重要的一条：

> Kernel 必须在第一次可能把请求交给 Provider 之前，先留下 durable DISPATCHED 事实。

因此允许一个保守窗口：

```text
DISPATCHED 已持久化
→ Kernel crash
→ 实际可能尚未真正发送
```

恢复后可以是 Unknown。

这是允许的，因为：

> 宁可承认不知道，也不能为了"看起来恢复成功"而偷偷重复真实副作用。

本文件不设计更复杂的 reconciliation。

## 5. Invocation ID ownership

> Runtime 在提交前生成稳定 Invocation ID；
> Kernel 权威拥有的是这个 Invocation 的**不可变绑定与状态**，而不是 ID 字符串的生成权。

不引入独立的 request_key。

## 6. 请求内容不可变，但不冻结实现方式

语义要求：

> I88 第一次登记以后，对应的 capability binding 和真正提交给 Provider 的请求内容
> 不可改变。

但本文件不规定必须有 `args_digest`。允许未来实现：

- canonical args；
- immutable payload ref；
- payload + digest；
- 其它等价方式。

`args_digest` 只是实现手段，不是一级概念。

## 7. 极简心智模型

```text
Capability reference
= 你凭什么能碰

Invocation
= 你这一次到底碰了什么，以及现在已知结果是什么
```

**Kernel 核心承诺**：

> 有合法能力才能碰；
> 每一次碰都有稳定身份；
> 同一 Invocation 不能被偷偷改写或重复创建；
> 发生到哪里就如实记录到哪里；
> 不知道结果就说 Unknown。

**Kernel 不承诺**：

> 全世界副作用 exactly once。

## 8. 其它词全部降级

| 词 | 人话 | 地位 |
|---|---|---|
| Result | 回执 | 至少表达 Succeeded / Failed / Unknown，以及 opaque output / evidence ref |
| Actor | 谁在刷这张门禁卡 | 不是新架构概念；目标方向：Kernel 从可信 caller identity / credential 得知调用者，不依赖 Runtime 的 Run 对象告诉它"是谁" |
| correlation metadata | 日志备注 | 例如 session_id / run_id 可以记录、用于排查和关联历史，但**不能参与权限判断** |
| args_digest | 请求内容的指纹 | 实现手段，不是一级概念（见 §6） |

## 9. 极简调用图

```text
Runtime
"我已经决定做这件事"
     │
     │ submit(I88, capability_ref, args)
     ▼
Kernel
"检查引用 + 登记 I88"
     │
     │ dispatch
     ▼
Provider / External World
"真正执行"
     │
     ▼
Result
"回执（Succeeded / Failed / Unknown）"
     │
     ▼
Runtime
"继续下一轮思考"
```

## 10. Runtime / Kernel 边界

**Runtime 负责**：决定要不要做、做什么、参数是什么、生成稳定 Invocation ID、结果回来以后下一步怎么办。

**Kernel 负责**：Runtime 已经决定做以后，验证 capability reference，登记这笔 Invocation 并冻结其绑定与内容，可靠转发并留下可信、持久的状态记录。

**Kernel 不负责**：

- 判断这个计划聪不聪明；
- 判断这个行为是否符合 OKR；
- Context；
- Routing；
- Reflection；
- Coding 语义；
- Feishu 语义；
- Tool 描述；
- Prompt；
- Run 生命周期；
- 业务幂等 / 去重 / 重试 / reconciliation。

## 11. 当前实现与目标态的区别

以下内容全部是 **CURRENT IMPLEMENTATION FACT**，不是长期目标：

- operation 名；
- grants；
- approve_invocation；
- policy verdict；
- decision_id；
- Feishu 参数硬编码；
- Coding 专用路径；
- actor 来自 run.principal；
- idempotency key 使用 run/turn/tool index。

目标方向：

```text
Runtime:
  capability_ref + submit(I88, args)
        ↓
Kernel:
  validate reference
  atomically register I88 (freeze binding + content)
  dispatch
  return Result
```

具体迁移步骤不在本文件设计。

## 12. 为什么要保留 Kernel

> Runtime 为什么不直接调用 shell / Feishu / database？

因为：

> Runtime 是高变化、未来甚至可能被 Agent 自己修改的运行层。
> Kernel 是低变化的真实世界边界。

因此：

> Runtime 可以大胆进化，但没有对应 capability reference 就无法越过 Kernel 去碰真实资源。

这是 Kernel 长期存在的核心价值。

## 13. 和 Runtime V0 的关系

见 [`AGENT_RUNTIME_V0_SEMANTICS.md`](./AGENT_RUNTIME_V0_SEMANTICS.md)（不重复正文）：

```text
Session = 这段事情
Run = 这次开工
Context = 当前桌面

Run 内 Agent 决定真实动作
        ↓
capability reference
        ↓
Invocation
        ↓
Kernel
```

## 非目标（本文件不解决）

capability reference 的 acquisition / Authority / Approval migration / Policy Agent /
Registry migration / Runtime crate / Kernel crate / async invocation / Scheduler /
Trigger / Memory / Reflection / migration plan / 业务幂等框架 / reconciliation 框架。
