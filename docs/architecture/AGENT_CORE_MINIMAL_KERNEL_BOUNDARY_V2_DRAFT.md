# AGENT_CORE_MINIMAL_KERNEL_BOUNDARY_V2_DRAFT

状态：DRAFT
目的：重新收敛 Agent Core 的 Kernel / Agent / Harness 边界。
原则：以 Linux Kernel / Userspace 的分层思想作为长期主要参照，但不机械复制 Linux 的具体实现。

---

# 1. 北极星

用户从飞书给一个持久 Agent 一句话。

Agent 应该能够自己：

* 理解需求
* 调查历史
* 调查代码
* 使用通用工具
* 修改代码
* build / test / debug
* 创建或修改外部 Harness
* 验证结果
* 从过去历史中总结经验
* 改变以后自己的行为

Kernel 不负责教 Agent 怎么完成这些事情。

长期自进化依赖下面这个闭环：

```text
Agent 行动
→ 外部 Harness / Tool 执行
→ 留下可追溯历史
→ Agent / Reviewer / Memory 复盘
→ 总结经验
→ 修改 Agent 行为 / Skill / Harness / 外部代码
→ 下一轮表现改变
```

核心假设：

> 没有长期历史，就很难产生可靠的长期自我改进。

因此系统必须优先保证“过去真实发生过什么”能够被重新找到。

---

# 2. Linux 是默认架构参照

Agent Core 长期参考 Linux 的核心原因不是 Linux“小”，而是它有一个很重要的边界思想：

> Kernel 提供稳定机制和边界接口；大量具体行为、工具和变化发生在 Kernel 外。

Agent Core 应持续用下面这些 Linux 类比检查自身：

```text
Linux process / uid
≈ Agent / Principal

syscall
≈ capability invocation

kernel bookkeeping
≈ Run / Invocation / Journal

userspace program
≈ Agent / Harness

driver / external subsystem
≈ external capability provider

audit / tracing
≈ Journal + Harness evidence

sandbox / namespace / Landlock / seccomp 一类机制
≈ Execution Runtime 的环境边界
```

这些只用于帮助判断职责，不要求一一对应。

特别禁止因为类比 Linux 而推出：

> “Linux Kernel 里有这个，所以 Agent Core Kernel 也必须有。”

真正要学习的是：

> Kernel 是否只掌握它不可替代的稳定机制，而不是具体产品语义。

---

# 3. 三个主要角色

## 3.1 Agent：决定“做什么、怎么做”

Agent owns behavior。

包括：

```text
理解需求
推理
规划
选择工具
看代码
写代码
测试策略
debug
决定下一步
总结经验
改变未来行为
```

Coding 属于 Agent。

不存在这样的长期架构原则：

```text
Coding Harness 决定怎么 Coding
```

---

## 3.2 External Harness / Tool：真的把事情做出来

Harness owns execution。

例如 Generic Execution Runtime 可以提供：

```text
workspace
filesystem
shell
git
compiler
test runner
process execution
HTTP client
资源限制
隔离环境
```

其他 Harness 可以提供：

```text
route
deployment
failure evidence
context
memory
其他外部能力
```

Harness 可以保存丰富执行历史，例如：

```text
command
cwd
stdout / stderr
diff
process tree
network trace
test log
resource usage
```

这些详细数据不要求全部进入 Kernel。

---

## 3.3 Kernel：稳定的调用交界点 + 可信历史骨架

Kernel 不负责理解 Agent 正在干什么业务。

Kernel 只负责非常机械的问题：

```text
是谁？
属于哪个 Session / Run？
它调用的是哪个已登记 capability？
这次调用的 Invocation ID 是什么？
根据当前有效授权/决策，是否允许把调用送出去？
结果属于哪个 Invocation？
可信证据在哪里？
```

Kernel 更像：

> 一个非常笨但可靠的 syscall boundary + bookkeeping layer。

而不是：

> 公司安全总监、研发经理、工作流引擎或产品大脑。

---

# 4. Kernel 明确不应该知道什么

以下概念不得因为当前产品需要而进入 Kernel：

```text
Route Harness
Coding
Memory
Context 产品语义
Failure Viewer
Workflow
Task
Job
Progress
Checkpoint
GitHub
Feishu 业务规则
数据库业务规则
某个具体 Agent 的产品角色
```

同时，Kernel 不应该理解环境的业务价值：

```text
这是生产环境
这是测试环境
这是公网
这是内网
这是真实资产
这是非真实资产
这个 API 很危险
这个 URL 很安全
这个 shell command 是部署
```

这些属于 Harness / Infra / 外部 Policy / Capability Owner 的知识。

Kernel 不能依赖理解这些语义才能正确工作。

---

# 5. 删除 “Effect Governance” 作为 Kernel 一级职责

V2 不再使用：

```text
Kernel owns Effect Governance
```

因为这个词很容易演变成：

```text
Kernel 理解网络
Kernel 理解生产
Kernel 理解数据库
Kernel 理解部署
Kernel 判断某个具体行为危险程度
```

这些都不是干净 Kernel 应有的知识。

替换为：

# Invocation Mediation

人话：

> Agent 要调用 Kernel 外的某个能力，Kernel 把这次调用可靠地串起来。

例如：

```text
Actor P
Run R
调用 Capability C
→ Invocation I
→ Decision D
→ Provider H
→ Result X
→ Evidence E
```

Kernel 不需要知道 Capability C 背后的业务意义。

---

# 6. Kernel 最小概念集合（当前草案）

目前只认为以下概念有较强理由留在 Kernel。

但仍允许后续继续削减。

## 6.1 Actor / Principal Reference

人话：

> 这次动作是谁发起的？

Kernel 不负责创建身份、密码、Client Secret 或登录。

这些属于外部 Auth。

Kernel 只需要可信地绑定：

```text
Invocation I
由 Principal P 发起
```

Linux 类比：

> Kernel 不负责决定“yanfenma 是什么样的人”，但执行 syscall 时必须知道当前 credential / uid 属于谁。

---

## 6.2 Session

人话：

> 这是哪一段连续经历？

Session 让持久 Agent 的长期工作有连续性。

它不是 Workflow。

---

## 6.3 Run

人话：

> 这一轮执行是哪一轮？

例如同一个 Session：

```text
Run 1：调查
Run 2：继续开发
Run 3：测试
```

Run 只作为时间线边界，不演化成：

```text
Task
Job
Progress system
```

未来如果 Agent Loop 完全外移，Run 是否仍应由 Kernel owns，需要重新审查。

---

## 6.4 Capability

人话：

> Kernel 外面有一个可以调用的入口。

Kernel 可以知道：

```text
capability_id=C123
provider=P456
version=v1
```

但不需要理解：

```text
C123 实际上是 shell
route
deploy
发消息
还是别的东西
```

display name 可以有人类语义，但 Kernel logic 不能依赖该名字。

---

## 6.5 Invocation

这是目前认为 Kernel 最不可替代的概念之一。

人话：

> 某个人，在某一轮，对某个能力，实际发起了一次调用。

例如：

```text
Invocation I123
principal=main
run=R7
capability=C55
arguments_digest=...
started_at=...
result=...
evidence_ref=...
```

它类似 syscall / I/O request 的一次具体发生。

---

## 6.6 Journal

人话：

> Kernel 自己的黑匣子。

记录稳定、可信的时间线骨架：

```text
Run 开始
Invocation 创建
Decision 返回
Invocation 发出
Result 收到
Run 完成 / 失败
```

Kernel Journal 不追求保存所有执行细节。

---

## 6.7 Evidence Reference

Kernel 可以保存：

```text
evidence_ref=E123
evidence_digest=sha256:...
```

真正丰富的 evidence 可以存在 Harness。

例如 Execution Harness 保存：

```text
cargo test 全量日志
git diff
shell stdout
HTTP trace
```

Kernel 只保存：

```text
Invocation 成功
evidence=E123
digest=...
```

原则：

> Kernel 保存总账，Harness 保存附件。

---

# 7. Receipt 不再强制作为独立一级概念

Receipt 可以理解成：

> Invocation 的可信结果。

例如：

```text
Invocation I123
result=success
evidence_ref=E55
```

不要求：

```text
每次 cat / grep / read
都产生一套重量级 Receipt protocol。
```

对于真正需要更强恢复保证的 Capability，可以由该 Capability / Harness 定义更丰富的 Receipt。

Kernel 只需要能够稳定关联：

```text
Invocation
→ Result
→ Evidence
```

是否继续保留当前 `Receipt` 实现属于后续实现收敛问题，不代表它必须永远是一个独立的产品级抽象。

---

# 8. History 和 Context 必须分开

这是长期自进化的核心边界。

```text
History
= 过去真实发生过什么
= 尽量长期保存

Context
= 这一轮需要给模型看什么
= 可以选择、压缩、总结、丢弃
```

不能因为 Context compaction 而删除 History。

Kernel Journal 保存可信历史骨架。

Harness 保存丰富执行历史。

Memory / Reflection Agent 可以读取两者，形成：

```text
经验
总结
行为规则
Skill
Memory
外部代码修改
```

Kernel 不负责总结历史。

Kernel 是记录者，不是学习者。

---

# 9. Generic Execution Runtime 的边界

Coding 属于 Agent。

执行属于通用外部 Execution Runtime。

系统长期只应该有一个底层 execution authority，避免：

```text
Coding Harness 一套 shell/git/build
Execution Harness 又一套 shell/git/build
```

目标形态：

```text
Persistent Agent
       ↓
Generic Execution Runtime
       ↓
workspace / filesystem / shell / git / build / test
```

Coding Agent、Route 工作、Failure Viewer 修改等都可以使用同一执行底座。

Execution Runtime 不知道：

```text
这是 Route 开发
这是 Failure 开发
这是 Memory 开发
```

---

# 10. workspace.exec 不由 Kernel 理解其内部行为

Kernel 不分析：

```text
curl
git
python
cargo
npm
```

Kernel 不判断：

```text
这个 curl 是公网还是内网
这个 URL 是不是生产
这个命令是不是危险
```

这些属于 Execution Runtime / sandbox / Infra。

正确模型：

```text
Kernel
只知道：
main 调用了 Capability C123
Invocation=I55

Execution Runtime
知道：
命令是什么
cwd 是什么
进程访问到了哪里
filesystem / network / credential 环境是什么
```

因此：

> 不治理命令本身，治理执行环境。

Agent 仍然可以正常：

```text
curl localhost
curl 开发 API
运行测试服务器
git
cargo
npm
```

能访问什么由它所在 Execution Environment 的真实边界决定。

Kernel 不维护“公网/生产/真实资产”分类。

---

# 11. Execution History 不等于 Kernel History

workspace.exec 不能成为不可观察黑洞。

但解决办法不是 Kernel 解析 shell。

而是：

```text
Kernel：
记录 Invocation + Result + EvidenceRef

Execution Harness：
记录详细 execution trace
```

未来 Reflection 可以：

```text
先从 Kernel 找相关 Run / Invocation
→ 根据 EvidenceRef 找 Harness 原始证据
→ 总结经验
```

目标是：

> 可追溯，而不是 Kernel 全知。

---

# 12. 当前 Generic Capability 注册问题

当前 PoC 证明：

真实持久 Agent `main` 可以通过通用 Execution Harness 完成：

```text
filesystem
shell
git
build/test
local process
HTTP probe
```

并且可以完全不调用：

```text
external.coding_task_submit
```

因此：

```text
GENERIC_EXECUTION_POC=PASS
```

但目前为了通过 Kernel 的静态 grant allowlist，Execution Harness 使用了：

```text
external.coding_workspace_*
```

名字。

这是临时兼容方式，不是最终架构。

真实语义是：

```text
generic execution
```

却借用了：

```text
coding
```

的旧名字。

因此此实现目前属于：

```text
POC
```

而不是正式冻结接口。

在 generic capability / grant 机制厘清前，不应把这一兼容关系永久固化。

---

# 13. 当前 Kernel 权限模型仍是 OPEN QUESTION

尚未冻结：

```text
谁决定一个 Agent 能不能调用某个 Capability？
```

可能方案包括：

```text
A. Kernel 内部 Policy 决定

B. 外部 Policy / Approval Harness 决定，
   Kernel 只负责 enforcement

C. Auth / Capability Owner 预先生成 Grant，
   Kernel 只检查 Grant

D. 上述模型的更小组合
```

下一轮必须继续检查：

```text
Policy
Approval
Decision
Grant
Registry
```

哪些真的是 Kernel 不可替代能力。

不能因为当前代码已经存在就默认保留。

特别需要用 Linux 做对照：

```text
哪些类似内核的 credential / syscall enforcement？

哪些更像 userspace policy daemon？

哪些更像 LSM hook？

哪些只是当前产品实现偶然长进 Kernel 的？
```

---

# 14. 判断某个概念是否应该进入 Kernel 的新规则

废弃旧规则：

```text
重要 → Kernel
需要持久 → Kernel
涉及权限 → Kernel
涉及副作用 → Kernel
```

这些推理都不成立。

新的判断只问三个问题。

## A. Kernel 完全不知道它，Kernel 自己还能不能正确工作？

如果能：

→ 优先放外面。

例如 Kernel 不知道：

```text
production
GitHub
公网
Route
cargo
数据库
```

完全不影响 Kernel 自己正确工作。

---

## B. Kernel 能不能完全不理解业务语义，只用稳定 ID / 状态处理它？

例如：

```text
Principal P
Capability C
Invocation I
Decision D
Result R
Evidence E
```

可以。

这种机制才可能适合 Kernel。

---

## C. 这件事是否只有 Kernel 这个交界点才能可靠完成？

例如：

```text
给进入 Kernel 的 Invocation 分配唯一 ID
绑定到当前 Run
按顺序写 Kernel Journal
将结果重新关联到原 Invocation
```

Kernel 天然处于正确位置。

而：

```text
shell filesystem sandbox
network restriction
test strategy
Route mapping
```

都有更合适的外部 owner。

原则：

> 不是“重要就进入 Kernel”。

而是：

> 只有 Kernel 才做得对，才进入 Kernel。

---

# 15. 当前架构一句话

Agent Core V2 当前最接近的模型：

```text
Agent
负责决定与学习

Kernel
负责稳定的调用边界和可信历史骨架

Harness
负责执行、环境边界和丰富证据
```

进一步说：

> Kernel 记录“谁通过哪扇门发起了一次调用”，但不需要知道门后面是什么。

---

# 16. 当前必须继续讨论、尚未冻结的问题

下一轮依次挑战：

```text
1. Approval 是否还应该属于 Kernel？

2. Policy 是否应该在 Kernel？
   还是 Kernel 只保留一个 decision hook / enforcement point？

3. Grant 谁签发、谁保存、谁验证？

4. Capability Registry 是否真的应该由 Kernel owns？
   还是 Kernel 只持有当前已激活 snapshot？

5. ToolCatalog 是 Kernel 的职责，
   还是 Agent Runtime 根据 snapshot 生成？

6. Session 是否必须 Kernel-owned？

7. Run 是否最终可以外移？

8. 当前 Receipt / Decision / Approval 数据结构
   有多少只是历史实现负担？
```

任何一个当前已经存在于 Kernel 的模块，都不能因为：

```text
“已经写了”
“测试很多”
“以前冻结过”
```

而自动获得永久存在资格。

---

# 17. 当前冻结程度

当前文档：

```text
AGENT_CORE_MINIMAL_KERNEL_BOUNDARY_V2_DRAFT
```

不是最终 Frozen Boundary。

已经高度确定：

```text
Agent owns behavior/coding
Harness owns execution
Kernel 不理解产品语义
Kernel 不理解网络/生产/资产分类
删除 Effect Governance 作为 Kernel 一级概念
History ≠ Context
Kernel history = 可信骨架
Harness history = 丰富证据
generic execution PoC 方向成立
```

仍未确定：

```text
Approval
Policy
Decision
Grant
Registry
Session / Run 的最终最小形态
```

在这些问题完成进一步削减前：

```text
V2_DRAFT != FROZEN
```
