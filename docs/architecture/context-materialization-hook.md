# Context Bounding & External Context Hook — 上下文限界与外部 Context Hook 边界

> **Status**: Architecture decision record
> **Date**: 2026-07-26
> **Audience**: Agent Core Kernel and External Harness / Context Provider developers

---

## 规范性决策与说明性内容

本文档分为两部分：

| 部分 | 标记 | 内容 | 约束力 |
|------|------|------|--------|
| **规范性决策 (Normative Decisions)** | 章节标题含 `[Normative]` | Kernel 职责、Hook 边界、负面约束、实施顺序。所有必须遵守的决策 | **必须遵守**。Kernel PR 违反即不通过 |
| **说明性内容 (Informative Examples / Appendix)** | 章节标题含 `[Informative]` | 背景数据、示例 Schema、外部 Provider 策略清单、真实观测值 | **仅供参考**。可在不修改 Kernel 的前提下自由更新 |

---

## 目录

1. [问题背景 [Informative]](#1-问题背景-informative)
2. [Kernel 应负责的机制 [Normative]](#2-kernel-应负责的机制-normative)
3. [外部 Context Provider 应负责的策略 [Informative]](#3-外部-context-provider-应负责的策略-informative)
4. [现有 Hook ABI 盘点 [Informative]](#4-现有-hook-abi-盘点-informative)
   - 4.5 [现有 Hook 的固定语义 [Normative]](#45-现有-hook-的固定语义-normative)
5. [Hook 能力要求 [Normative]](#5-hook-能力要求-normative)
6. [Hook 输入输出的最小形态 [Normative]](#6-hook-输入输出的最小形态-normative)
7. [Kernel 的机械 fallback [Normative]](#7-kernel-的机械-fallback-normative)
8. [禁止事项 [Normative]](#8-禁止事项-normative)
9. [实施顺序 [Normative]](#9-实施顺序-normative)
10. [防跑偏验收 [Normative]](#10-防跑偏验收-normative)

---

## 1. 问题背景 [Informative]

### 观测到的真实问题

在长工具链场景中，当前的上下文材料化策略将全部历史 tool-call / tool-result 持续回灌给模型：

- 单次模型调用中的 input tokens 约 **80K**
- 其中工具结果约占 **215KB** 原始文本
- 模型响应时间因上下文膨胀多次达到 **30 秒超时阈值**
- follow-up 请求的上下文持续增长，没有自然的裁剪边界

小上下文请求也可能遇到模型偶发慢响应。因此：

> **上下文必须有限；模型错误必须准确分类；单纯提高超时不是长期解决方案。**

### 现有能力 vs 缺失

项目已有：

- `context.prepare.v0` Hook — 外部 Harness 在模型调用前注入动态上下文片段（指令、事实、引用等），**已实现并活跃**
- `context.compress.v0` Hook 类型 — 在 `HookKind` 枚举中已定义，**未实现**
- `ContextFragment` / `ResourceRef` 类型体系 — 已定义
- Kernel 侧的基础上下文组装管线（`src/runtime/hook_call.rs`、`src/domain/context_block.rs`）
- 运行配置中的 `context_budget_chars`

缺失：

- **Kernel 机械预算**: 模型上下文没有硬性上限；完整工具结果持续回灌而不裁剪
- **tool-call / tool-result 成对完整性**: 没有机制保证发送给模型的消息不会出现孤立的 tool message
- **大结果 preview/ref 机制**: 完整工具结果保持在 Journal / ContentStore 中，但模型只接收有限 preview 和引用
- **Context Hook 调用点**: 当前 `context.prepare.v0` 的定位是注入额外上下文，不是"在预算内裁剪已有上下文"
- **机械降级路径**: 外部 Hook 失败时没有确定的缩减策略

### 设计目标

```text
外部 Agent / Context Provider
→ 自由迭代摘要、检索、裁剪、Repo Map、任务状态提取等策略

Kernel
→ 只提供稳定的上下文生命周期 Hook、预算约束、事实引用和失败兜底
```

核心原则：

> Kernel 不负责压缩得聪明，但必须提供足够通用的 Hook，让外部策略可以持续进化；
> 同时 Kernel 必须保证无论外部 Hook 是否可用，
> 发送给模型的上下文都有限、合法、成对、可追溯。

---

## 2. Kernel 应负责的机制 [Normative]

### 2.1 职责清单

Kernel **只负责**以下上下文生命周期原语：

| # | 职责 | 说明 |
|---|------|------|
| 1 | **检测和执行模型上下文预算** | 在组装最终模型请求前计算总 token 估计值，如果超出配置上限则执行降级 |
| 2 | **完整保存原始内容** | Journal / Tool Receipt / ContentStore 中的原始数据必须完整持久化，不能因裁剪而丢失 |
| 3 | **保证 tool_call 与 tool_result 成对** | 发送给模型的消息序列中，任何 `tool_call` 必须有对应的 `tool_result`（或明确的替代标记），不允许出现孤立的 tool message |
| 4 | **保留不可变上下文** | 必要 system prompt、当前用户请求、治理上下文（权限、安全规则等）不受裁剪影响 |
| 5 | **提供通用 Context Hook 调用点** | 在模型请求材料化（materialize）前提供同步调用点，让外部 Provider 有机会参与上下文决策 |
| 6 | **验证 Hook 返回结果** | 验证返回引用的 session/run 边界、digest 一致性、预算合规性 |
| 7 | **Hook 失败时机械降级** | 外部 Hook 超时、失败或返回非法结果时，使用确定性的内置策略确保模型调用仍可进行 |
| 8 | **记录 Context Materialization 来源和结果** | 每次模型请求的上下文组装过程必须可审计：哪些内容来自 Hook、哪些是 fallback、预算占用多少 |

### 2.2 Kernel 不得理解

Kernel **不得理解**以下任何语义性策略：

| 禁止理解的内容 | 原因 |
|---------------|------|
| 哪段代码重要 | 这是 Harness/Provider 的任务语义判断 |
| 哪个错误应保留 | 摘要策略由外部决定 |
| 测试做到哪一步 | 开发任务状态是产品层概念 |
| 开发任务当前阶段 | 同左 |
| 应使用什么摘要 Prompt | 摘要内容是外部策略的实现细节 |
| 应选择摘要、检索、Repo Map 还是其他策略 | 策略选择是 Provider 的职责 |

---

## 3. 外部 Context Provider 应负责的策略 [Informative]

### 3.1 策略清单

外部 Context Provider（通过 Hook 接入）可以自由演进以下策略：

| 策略 | 说明 |
|------|------|
| **语义摘要** | 对长工具结果、代码输出、日志进行模型驱动的摘要 |
| **相关事件选择** | 只选择与当前请求语义相关的历史事件 |
| **工具结果裁剪** | 去掉冗余、重复或低价值的工具输出 |
| **项目结构 / Repo Map** | 提供当前项目的文件结构概览 |
| **当前任务状态提取** | 从历史中提取开发者当前所处的任务阶段 |
| **错误日志关键片段提取** | 从大量错误输出中提取根因和关键栈帧 |
| **不同模型的上下文适配** | 为不同模型（不同 context window、不同指令遵循能力）提供适配后的上下文 |
| **历史检索和选择性回放** | 从长历史中选择性检索和重构上下文 |

### 3.2 非绑定约束

- 更换上述任何**策略不得要求修改 Kernel**
- 策略的 Prompt、算法、模型选择完全由 Provider 决定
- 多个 Provider 可以并行存在（通过 Hook 注册表，未来能力）
- Provider 可以是本地进程、远程服务或内联函数

---

## 4. 现有 Hook ABI 盘点 [Informative]

### 4.1 已实现状态

| Hook 类型 | 状态 | 说明 |
|-----------|------|------|
| `ingress.route.v0` | 仅类型定义 | 路由决策 |
| `context.prepare.v0` | **已实现并活跃** | 模型调用前注入动态上下文片段 |
| `context.load.v0` | 仅类型定义 | 渐进式资源加载 |
| `context.compress.v0` | 仅类型定义 | 上下文压缩/摘要 |
| `event.observe.v0` | 仅类型定义 | 外部 Harness 的事件观察 |
| `decision.policy.v0` | 仅类型定义 | 决策策略 |

### 4.2 `context.prepare.v0` 当前能力

**输入** (`ContextPrepareRequest`):

```rust
pub struct ContextPrepareRequest {
    pub hook: HookKind,
    pub run_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub principal: String,
    pub channel: String,
    pub user_text: String,
    pub context_budget_chars: usize,
}
```

**输出** (`ContextPrepareResponse`):

```rust
// 通过 HookResponseEnvelope.payload 传递，包含:
// - fragments: Vec<ContextFragment>  // 注入的上下文片段
// - resource_refs: Option<Vec<ResourceRef>>  // 渐进式披露引用
```

**运行时集成**：
- 在 `Runtime::deliver()` 的上下文组装之后、首次 LLM 调用之前同步调用
- 结果以 `HookFragment` ContextBlock 形式注入到 `UserMessage` 块之前
- 失败时根据 `HookFailureMode` 处理（`FailClosed` 终止 Run，`FailOpen`/`Degrade` 继续）

### 4.3 能力缺口（Gaps）

| 缺口 | 描述 |
|------|------|
| **Gap A —— 缺少"已有上下文的裁剪"视角** | 现有 `context.prepare.v0` 是**追加**模型——它注入新片段，但不参与对已有 Journal 内容的裁剪决策。Context Bounding 需要的不是"加什么"，而是"保留什么、裁掉什么" |
| **Gap B —— 缺少 through_event_id 边界** | 当前请求不传达"本次模型调用应覆盖到哪个事件为止"。Hook 无法知道其决策范围的终点 |
| **Gap C —— 缺少已有内容引用** | Hook 无法得知 Kernel 当前已计划发送哪些 tool-call / tool-result 对，因此无法做精确预算分配 |
| **Gap D —— `context.compress.v0` 未实现** | 定义为压缩用途，但尚无运行时调用点。其定位恰好匹配"在预算内对已有上下文做策略性处理"的诉求 |
| **Gap E —— 输出缺少 ContextPlan** | Hook 的返回是平铺的 fragments 列表，无法表达"保留 X、裁剪 Y、摘要 Z"的结构化计划 |
| **Gap F —— 缺少通用上下文范围引用** | 外部 Provider 难以获知本次 Context Materialization 可以访问的资源范围。缺少 `context_scope_refs` / `resource_refs` / `subject_refs` 等不透明引用，Provider 无法判断可检索的资源边界。这些引用对 Kernel 是不透明的——Kernel 只验证引用的权限和归属，不理解它代表 workspace、repository、mailbox 或其他业务资源 |

### 4.4 结论

> **现有 Hook ABI（`context.prepare.v0`）可以复用为上下文限界的通信通道，但其输入输出契约需要扩展。**
>
> 不推荐新建一个平行 Hook 类型专用于 bounding——`context.prepare.v0`（或未来实现的 `context.compress.v0`）加上扩展字段即可覆盖该能力。

### 4.5 现有 Hook 的固定语义 [Normative]

本轮冻结 `context.prepare.v0` 和 `context.compress.v0` 的职责边界，不得新增第三个平行的 `context.materialize.v0`。

**`context.prepare.v0`**

| 方面 | 约束 |
|------|------|
| 职责 | 在 Kernel 已选的基础上下文之上增量补充外部片段（检索结果、Repo Map、长期记忆等） |
| 限制 | **不得删除或替换** Kernel 已选内容。只做加法 |
| 预算责任 | 不负责解决超预算。Fragment 的 `estimated_tokens` 仅供 Kernel 参考，超出预算时由 Kernel 或 `context.compress.v0` 处理 |
| 调用条件 | 每次模型调用前执行，不受预算压力影响 |

**`context.compress.v0`**

| 方面 | 约束 |
|------|------|
| 职责 | 仅在上下文压力达到预算阈值时参与预算规划。可选择、替换、摘要历史上下文 |
| 输出 | 返回 `ContextPlan` / `ContextArtifact` 等结构化计划 |
| 限制 | **不得修改或删除** 原始 Journal / Receipt / ContentStore 内容。ContextPlan 只影响"发送给模型的视图"，不影响持久化数据 |
| 调用条件 | 仅在检测到预算压力时由 Kernel 按需调用 |

**固定调用顺序**

```text
Kernel 机械选择基础上下文
→ context.prepare.v0 可选增量补充
→ 检测预算压力
→ 必要时调用 context.compress.v0
→ Kernel 验证预算、不变量和引用
→ 调用模型
```

无论哪个 Hook 失败（超时、崩溃、返回非法结果），Kernel 均使用 [机械 fallback](#7-kernel-的机械-fallback-normative) 继续执行，不阻塞 Run。

---

## 5. Hook 能力要求 [Normative]

### 5.1 能力清单

在模型请求 materialize 前，Kernel 提供的 Hook 调用点必须支持以下能力：

| # | 能力 | 说明 |
|---|------|------|
| 1 | **同步调用外部 Provider** | 在上下文最终组装前执行一次同步调用；异步编排属于 Provider 内部实现 |
| 2 | **传出 Session/Run 标识** | 传递 `session_id`、`run_id`、`agent_id`。不包含业务语义标识（如 workspace_id） |
| 3 | **传出上下文范围引用** | 传递 `context_scope_refs`、`resource_refs`、`subject_refs` 等不透明引用，Provider 可凭此判断可检索的资源边界。Kernel 只验证引用的权限和归属，不理解其业务含义（workspace、repository、mailbox 等） |
| 4 | **传出 Journal 上界** | 传递 `through_event_id`——本次模型调用应覆盖到的最大事件序号 |
| 5 | **传出模型预算** | 传递 `model_budget_chars` 或 `model_budget_tokens`——本次调用的 Token 上限 |
| 6 | **通过引用暴露内容** | 不传递原始 Journal / ContentStore 全文，而是通过 `event_refs` / `result_refs` 引用让 Provider 选择性地拉取 |
| 7 | **接收结构化 Context Plan** | Provider 返回的不是平铺文本，而是包含"保留/裁剪/摘要/引用"的结构化计划（ContextArtifact / ContextPlan） |
| 8 | **验证引用边界** | 验证返回的引用都属于当前 Session/Run，不引用未来的事件 |
| 9 | **验证不超预算** | 验证 Provider 返回的上下文计划总估计 Token 不超过预算 |
| 10 | **Hook 失败时 Kernel fallback** | Provider 超时、崩溃或返回非法结构时，Kernel 使用内置机械降级 |

### 5.2 复用已有 ABI，不新增平行框架

> **推荐语义名称仍为 `context.prepare.v0`（扩展）或未来激活 `context.compress.v0`，不新增 `context.bounding.v0` 或 `context.materialize.v0` 等平行命名。**
>
> 理由：已有 Hook ABI（`hook/types.rs` 中的 `HookKind`）和 `HookClient` trait 足以表达同步调用点。扩展已有类型的字段比新增一个平行调用框架的维护成本更低。

---

## 6. Hook 输入输出的最小形态 [Normative]

### 6.1 概念性契约

以下为概念契约，不要求本轮实现。具体字段名和序列化格式在实现时确定。

**Input**（发送给 External Provider）：

```text
session_id              — 当前会话 ID
run_id                  — 当前 Run ID
agent_id                — 当前 Agent ID
context_scope_refs      — 不透明引用，标识本次上下文材料化可访问的资源范围
resource_refs           — 不透明引用，标识可用的外部资源
subject_refs            — 不透明引用，标识相关主体
through_event_id        — 本次模型调用应覆盖到的最大事件序号
model_budget_chars      — 本次调用的总字符/Token 预算
recent_event_refs       — 最近 N 个事件的引用（id + kind + digest，不含完整内容）
available_result_refs   — 当前可用的工具结果引用（id + operation + status + digest + estimated_size）
required_context_refs   — Kernel 要求必须保留的上下文引用（system prompt、治理上下文等）
```

> Kernel 对 `context_scope_refs` / `resource_refs` / `subject_refs` 仅验证权限和归属，不理解其业务含义（workspace、repository、mailbox 等是外部 Provider 的内部概念，可作为 Provider 实现示例，但不得进入 Kernel 通用 ABI）。

**Output**（由 External Provider 返回）：

```text
through_event_id     — 确认的 through_event_id（必须 ≤ 输入值）
context_items        — 有序的上下文条目列表，每个条目可以是：
                       · RetainedEvent(id, reason)       — 保留完整事件
                       · SummarizedEvent(id, summary)    — 摘要后的事件
                       · TruncatedResult(id, preview)    — 截断的工具结果
                       · DroppedEvent(id, reason)        — 丢弃的事件
                       · InjectedFragment(fragment)      — 注入的新上下文片段
                       · ResultRef(id, digest, preview)  — 引用完整结果
digest               — 整个 ContextPlan 的内容摘要（用于审计和缓存）
estimated_tokens     — 计划的总预计 Token 数
provider_identity    — Provider 标识（用于审计和调试）
```

### 6.2 通用名称

外部返回物使用以下通用名称，不限制为 `CompactionSummary`：

```text
ContextArtifact    — 整个上下文计划的顶级容器
ContextFragment    — 上下文计划中的单个条目（与现有 `ContextFragment` 类型兼容扩展）
ContextPlan        — Provider 返回的结构化上下文编排计划
```

**设计意图**：未来实现可能不是简单的"摘要"（compaction），而可能是检索增强、Repo Map、选择性回放或其他策略。因此协议不应限制为 `CompactionSummary`。

---

## 7. Kernel 的机械 fallback [Normative]

### 7.1 原则

外部 Hook 不可用时（超时、崩溃、配置禁用、返回非法结果），Kernel **仍必须运行**。

Fallback 是 Kernel 的保底行为，不是主要策略。fallback 的存在是为了保证系统在外部依赖异常时仍然可用，不是为了替代外部 Provider。

### 7.2 Fallback 策略（第一版）

第一版机械降级策略是可配置的：

```text
1. 最近 N 组完整 tool-call / tool-result
   - 保留最近的完整工具调用和结果
   - N 是可配置运行参数（如 5 组）

2. 单条大工具结果只向模型提供有限 preview
   - 超过 M bytes 的工具结果只保留前 M bytes 作为 preview
   - 完整结果继续保存在 Journal / ContentStore 中
   - 模型可通过 ResourceRef 按需获取完整内容（当 context.load.v0 可用时）

3. 更早结果只提供 operation、status、digest、result_ref
   - 超出最近 N 组的早期结果缩减为元数据条目
   - 不丢失内容（Journal 中完整保留），只影响发送给模型的内容

4. 不允许产生孤立的 tool message
   - tool_call 和 tool_result 必须成对出现
   - 如果一对必须被整体丢弃，则都丢弃
   - 不允许只保留 tool_call 而丢弃对应的 tool_result
```

### 7.3 Fallback 参数

```text
N              — 保留的完整 tool-call/tool-result 组数（默认 5）
M              — 单条工具结果的 preview 字节上限（默认 4096）
budget_chars   — 模型调用的总字符预算（运行配置，非硬编码）
```

**文档明确**：

> `N`、`M` 和 `budget_chars` 是**运行配置**，不是长期语义策略。
> 它们的存在是为了在外部 Hook 不可用时保证系统可用性；
> 当外部 Provider 接入后，具体值由 Provider 的 ContextPlan 决定。

### 7.4 Fallback 优先级顺序

```text
1. 检查 Hook 是否已启用且可用
   ├─ 是 → 调用 Hook，验证返回
   │   ├─ 返回合法 → 使用 Hook 的 ContextPlan
   │   └─ 返回非法/超时 → 记录失败，进入 fallback
   └─ 否（禁用/未配置） → 进入 fallback

2. Fallback: 机械缩减
   ├─ 保留 N 组完整 tool-call/tool-result
   ├─ 单条结果超过 M bytes → preview + ref
   ├─ 更早结果 → metadata + ref
   └─ 保证 tool-call/tool-result 成对

3. 组装最终模型上下文
   ├─ 计算总预计 Token
   ├─ 超出 budget_chars → 进一步裁减（从最早开始）
   └─ 发送模型请求
```

---

## 8. 禁止事项 [Normative]

以下行为被明确禁止：

### 8.1 Kernel 侧禁止

```text
1. 不在 Kernel 内调用模型生成语义摘要
   → 摘要是外部 Provider 的策略职责，Kernel 不做模型调用

2. 不在 Kernel 内加入代码、测试、workspace 或开发任务特判
   → 这些都是产品层语义，不属于 Kernel 的通用原语

3. 不建立 CompactionRequest / CompactionJob / CompactionAttempt / CompactionRepair 状态机
   → 这只是一种实现方式，不应成为 Kernel 的通用契约
   → 外部 Provider 内部可以使用任何状态机

4. 不让 Context Hook 成为不可用就阻塞整个 Run 的安全边界
   → Hook 失败时 Kernel 必须通过机械降级继续执行
   → Hook 是用来优化上下文的，不是 Run 的安全必要条件

5. 不永久删除原始 Tool Receipt 或 Journal 内容
   → Kernel 的上下文材料化是"材料化一个视图"而不是"清理原始数据"
   → Journal / ContentStore 是 append-only 的，内容永不丢失

6. 不在模型超预算时同步等待一个长时间摘要任务
   → 如果 Hook 不能在时限内返回，Kernel 立即使用 fallback
   → 不允许让模型调用等待一个慢速的外部摘要

7. 不把固定的"8 轮/8KB"当作最终压缩算法
   → 当前的简单截断只是第一版 fallback
   → 长期目标是外部 Provider 的语义策略，不是 Kernel 的固定参数
```

### 8.2 外部 Provider 侧约束

```text
8. Provider 不得返回超出当前 Session/Run 边界的引用
   → Kernel 必须验证引用的 session_id / run_id 匹配

9. Provider 不得返回引用未来事件的 ContextPlan
   → through_event_id 必须 ≤ 输入值

10. Provider 不得返回总预算超过 model_budget_chars 的 ContextPlan
    → Kernel 必须验证总 estimated_tokens 在预算内
    → 超出预算的 ContextPlan 被拒绝，触发 Kernel fallback
```

---

## 9. 实施顺序 [Normative]

### Phase 1 —— Kernel 机械预算和错误分类

**目标**：在现有 Hook 基础设施上，Kernel 获得独立于外部 Provider 的上下文限界能力。

| 步骤 | 说明 |
|------|------|
| 1.1 | 准确区分 `model_timeout` / HTTP / transport / parse error，为超时归因提供基础 |
| 1.2 | Kernel 加入机械预算：在 `Runtime::deliver()` 的上下文组装末端计算总预计 Token，超出上限时执行机械降级 |
| 1.3 | Kernel 保证 tool-call / tool-result 成对完整性：引入消息序列校验，不允许孤立 tool message |
| 1.4 | 完整结果保留，模型只接收有限 preview/ref：大工具结果缩减为 preview + ResourceRef，完整内容保留在 ContentStore |

**可交付物**：
- 模型调用超时不再统一归为 `Timeout`，能区分 Provider 侧 vs 模型侧
- Agent Core 可以在**没有外部 Hook 的情况下**自行保证上下文有限且合法
- `Fallback` 路径可运行，有测试覆盖

### Phase 2 —— Hook ABI 扩展与首个外部 Provider

**目标**：扩展现有 Hook ABI 使其支持上下文限界，接入一个最简单的外部 Context Provider。

| 步骤 | 说明 |
|------|------|
| 2.1 | 盘点并补齐现有 `context.prepare.v0` ABI 的上下文限界能力（添加 `through_event_id`、`available_result_refs`、`required_context_refs` 等字段） |
| 2.2 | 实现或激活 `context.compress.v0` 的运行时调用点（如果架构决策认为两个 Hook 类型的职责拆分合理） |
| 2.3 | 实现 ContextPlan 输出类型的序列化和验证逻辑 |
| 2.4 | 接入一个最简单的外部 Context Provider（如基于规则的裁剪器），端到端验证 Hook 调用 → ContextPlan → 上下文组装的完整路径 |

**可交付物**：
- Hook ABI 扩展完成，输入输出契约对齐第 5、6 节定义
- 至少一个外部 Provider 可以成功参与上下文限界决策
- 机械 fallback 和 Hook 路径均可通过配置切换

### Phase 3 —— 外部策略自由迭代

**目标**：外部 Context Provider 自由迭代语义策略，Kernel 不随策略变化。

| 步骤 | 说明 |
|------|------|
| 3.1 | 外部迭代语义摘要（调用小模型对工具结果做摘要） |
| 3.2 | 外部迭代相关事件选择（从历史中选择语义相关的事件） |
| 3.3 | 外部迭代 Repo Map（提供当前项目结构概览） |
| 3.4 | 外部迭代模型专项适配（不同 context window 的模型使用不同策略） |
| 3.5 | Kernel 在任何策略迭代中不做代码修改 |

**可交付物**：
- 外部 Provider 的多个策略版本在同一个 Kernel 版本上运行
- 更换策略不需要 Kernel 发布

---

## 10. 防跑偏验收 [Normative]

任何与 Context 相关的 Kernel 修改**必须回答以下 7 个问题**：

| # | 问题 | 通过条件 |
|---|------|---------|
| 1 | **这是上下文稳定性原语，还是摘要策略？** | 只有原语可以进入 Kernel；策略必须通过 Hook 由外部提供 |
| 2 | **能否完全由外部 Context Provider 实现？** | 如果可以，则不应进入 Kernel；除非能否证明会导致不可绕过的安全或正确性问题 |
| 3 | **删除该 Kernel 修改会破坏什么不可替代的不变量？** | 答案必须是 Kernel 正确性不变量（如成对完整性、预算强制），而不是"外部 Provider 会更方便" |
| 4 | **是否让 Kernel 开始理解代码、测试或任务语义？** | 如果是，默认拒绝；只有证明是必要的安全原语才能例外 |
| 5 | **外部 Hook 挂掉后，Kernel 能否继续运行？** | 答案必须为"是"——任何 Context 修改不得使 Hook 成为绝对依赖 |
| 6 | **更换摘要/检索算法是否需要修改 Kernel？** | 答案必须为"否"——更换策略只需更换外部 Provider |
| 7 | **最小下一步是什么？** | 每个 Context PR 必须能回答并执行比当前更小的下一步，而不是一次性搭建完整体系 |

---

*End of document.*
