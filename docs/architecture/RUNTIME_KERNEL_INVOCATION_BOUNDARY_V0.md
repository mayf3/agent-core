# Runtime ↔ Kernel Invocation Boundary V0

> 状态：**SEMANTICS BASELINE V0**
>
> 冻结的是讨论语义基线，不代表当前 V1 implementation 已完成迁移。
> 本文不设计 Handle acquisition / Authority / migration。

## 0. 这份文档回答什么

> Runtime 已经决定「我要做一个真实外部动作」以后，Runtime 和 Kernel 之间到底发生什么？

整篇只有两个核心概念：**Handle** 和 **Invocation**。

## 1. 两个核心概念

### Handle —— 钥匙 / 门禁卡

表达：Runtime 已经获得了使用某项外部能力的资格。

Kernel 不需要重新理解：

- 这是 shell 还是 git；
- 这是 Coding 还是 Workflow；
- 这是生产还是测试；
- 为什么 Agent 想这么做。

Kernel 只机械检查：

- 这把钥匙是真的吗；
- 是当前调用者持有的吗；
- 是否过期；
- 是否撤销；
- 是否仍然绑定到有效能力。

（Handle acquisition / Authority 不在本文件范围。）

### Invocation —— 这一笔真实动作 / 流水号

Runtime 用 Handle 真正执行一次动作时：

```text
Handle H17
→ Kernel
→ Invocation I88
→ Provider
→ Result
```

Invocation 存在的原因：

- 唯一标识这一次真实动作；
- 可以追踪到底有没有执行；
- 支持幂等 / 重试；
- 可以关联结果和 evidence；
- 可以表达 outcome unknown。

## 2. 其它词全部降级

| 词 | 人话 | 地位 |
|---|---|---|
| Result | 回执 | 至少表达 Succeeded / Failed / Unknown，以及 opaque output / evidence ref |
| Actor | 谁在刷这张门禁卡 | 不是新架构概念；目标方向：Kernel 从可信 caller identity / credential 得知调用者，不依赖 Runtime 的 Run 对象告诉它"是谁" |
| retry / request key | 防止重试把同一件事做两遍的编号 | 只是调用字段；Kernel 不为了幂等理解 run_id / turn_index / tool_index 的业务意义 |
| correlation metadata | 日志备注 | 例如 session_id / run_id 可以记录、用于排查和关联历史，但**不能参与权限判断** |

## 3. 极简调用图

```text
Runtime
"我已经决定做这件事"
     │
     │ Handle + Args
     ▼
Kernel
"检查钥匙"
     │
     │ 创建 Invocation
     ▼
Provider / External World
"真正执行"
     │
     ▼
Result
"回执"
     │
     ▼
Runtime
"继续下一轮思考"
```

## 4. Runtime / Kernel 边界

**Runtime 负责**：决定要不要做、做什么、参数是什么、结果回来以后下一步怎么办。

**Kernel 负责**：Runtime 已经决定做以后，检查它有没有有效钥匙，给这一次真实动作编号，可靠执行 / 转发并留下可信记录。

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
- Run 生命周期。

## 5. 当前实现与目标态的区别

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
  valid Handle + Args
        ↓
Kernel:
  validate Handle
  create Invocation
  dispatch
  return Result
```

具体迁移步骤不在本文件设计。

## 6. 为什么要保留 Kernel

> Runtime 为什么不直接调用 shell / Feishu / database？

因为：

> Runtime 是高变化、未来甚至可能被 Agent 自己修改的运行层。
> Kernel 是低变化的真实世界边界。

因此：

> Runtime 可以大胆进化，但没有对应 Handle 就无法越过 Kernel 去碰真实资源。

这是 Kernel 长期存在的核心价值。

## 7. 和 Runtime V0 的关系

见 [`AGENT_RUNTIME_V0_SEMANTICS.md`](./AGENT_RUNTIME_V0_SEMANTICS.md)（不重复正文）：

```text
Session = 这段事情
Run = 这次开工
Context = 当前桌面

Run 内 Agent 决定真实动作
        ↓
Handle
        ↓
Invocation
        ↓
Kernel
```

## 非目标（本文件不解决）

Handle acquisition / Authority / Approval migration / Policy Agent / Registry migration / Runtime crate / Kernel crate / async invocation / Scheduler / Trigger / Memory / Reflection / migration plan。
