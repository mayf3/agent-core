# Decision: Phase 2 Invocation Gateway Hardening — scoping

Phase 1 Operational Hardening 完成（PRs #60–#70）。本文基于对 `src/` 的实际代码
核查，记录 Phase 2 "Invocation Gateway and Safe Tools" 的范围、当前缺口、以及最小
首批增量。**Analysis only — no `src/` change. Phase 2 实现需 maintainer 对每个增量
单独签字。**

## 背景

Product roadmap（`docs/product-roadmap.md`）Phase 2 目标：让 Agent 能做少量真实的、
有边界的动作，但不破坏小内核边界。内核新增：tool contract、intent schema validation、
run principal execution profile、fixed policy pipeline、approval state、第一批低风险
本地 adapter。

退出标准（roadmap）：每次外部动作都有 intent、decision、receipt、audit；read-only
操作可按 capability grant 执行；write/risky 操作需要 approval。

## 当前状态（main，已核查）

### Gateway（`src/gateway/mod.rs`）

- 无状态，只持有 `KernelConfig`。
- `validate_ingress`：检查 protocol/auth，按 source（cli/feishu）做 id 解析、allowlist、
  mention 检查、Journal dedupe。**principal 在这里硬编码内联构造**。
- `approve_invocation`（line 54-84）：**内联、同步、永远 approve**。三个检查：
  (a) grant 在 `run.principal.grants` 上；(b) operation 是两个硬编码字符串之一；
  (c) `target_session == session.id`。无 async pause、无 policy lookup。
- `recover_validated_event`：从 Journal payload 重建 `ValidatedEvent`。

`ApprovedInvocation`（`domain/mod.rs:232-249`）：持有 `intent` + `decision_id`
（纯 UUID 关联令牌，无 approver / verdict / expiry）。

### Principal / capability（`src/domain/mod.rs`）

- `RunPrincipal { principal_id, subject, source, grants: Vec<CapabilityGrant>, requester_id }`。
- `CapabilityGrant { operation: String, scope: String }`——operation 是裸字符串。
- **无 execution profile**：grep `execution_profile|ExecutionProfile` 零命中。principal
  在 gateway 里硬编码（cli 给 `stdout.send_text`/`current_session`；feishu 给
  `feishu.send_message`/`current_session`），每个 principal 恰好一个 grant。
- `Runtime::create_run` 直接 clone `event.principal`，不从 channel/config 重新派生。

### Approval / pause —— **不存在**

- grep `pause|resume|pending_approval` 零命中。approval 纯粹是 `Runtime::deliver` 里的
  同步 `approve_invocation(...)?`，通过即立即 `queue_outbox_dispatch`。
- 无 `AwaitingApproval` run status；`decision_id` 只是关联 UUID。
- 无 `ApprovalRequest`/`ApprovalDecision` 持久记录。

### Adapters（`src/adapters/mod.rs`）

- `InvocationAdapter` trait：单方法 `execute(&ApprovedInvocation) -> Result<Receipt>`，
  无 lifecycle、无 pre/post hook、无 I/O schema。
- `HttpConnectorAdapter`：POST 到 localhost connector，固定 10s 超时。**2xx 永远映射成
  `ReceiptStatus::Succeeded`——它从不读响应里的 status 字段**（line 44-53）；non-2xx
  → `bail!("connector execute failed")`。
- `StdoutAdapter`：stub，读 `text`，返回 `Succeeded`，**从不真正打印**。
- `ReceiptStatus::{Succeeded, Failed, Unknown}`。

### Policy / hook —— **不存在**

- 无 pluggable policy pipeline，无 deny/allow verdict enum，无 argument transform。
- 唯一的"policy"是 `approve_invocation` 里的 3-clause 内联检查 + config 里的 feishu
  allowlist 向量。

### Operations / tool contracts —— 硬编码字符串，无 registry

- 只有 `stdout.send_text` 和 `feishu.send_message` 两个 operation，作为裸字符串出现在
  11 处（gateway/runtime/adapters/tests）。
- 无 operation registry、无 schema、无 catalog。"allowlist" 是字面量 `if intent.operation
  != "stdout.send_text" && ... != "feishu.send_message"`。
- argument 校验是 gateway 里手写的 `string_arg(&intent.arguments, "message_id")`。
- surfacing 给 LLM 的只有一个静态 context block（`context.rs:38-43`），无 tool schema。

## Phase 2 缺口清单（对照 roadmap）

| Roadmap 项 | 当前状态 | 缺口 |
|---|---|---|
| tool contract | 11 处裸字符串 | operation catalog 类型 + 单一事实源（gateway allowlist / arg 校验 / adapter dispatch 都引用它） |
| intent schema validation | `arguments: Value` 无类型，手写 `string_arg` | 每 operation 的 argument schema + 拒绝/接受契约 |
| run principal execution profile | gateway 硬编码单 grant | profile 类型，按 channel + config 派生 grants/limits |
| fixed policy pipeline | 3-clause 内联 `bail!` 阶梯 | 有序 pipeline（deny → allow → transform）+ verdict 类型 |
| approval state | 同步无状态，UUID 关联 | 持久 approval state（Journal fact + `AwaitingApproval` run status + resume path） |
| first batch low-risk local adapters | `StdoutAdapter` 是 stub | 真正的本地 adapter（fs read / stdout write / time）+ 共享契约 |

### 横切风险（必须在 Phase 2 早期处理）

**错误分类全靠字符串子串匹配**（`DispatchErrorCategory::from_error`，
`domain/mod.rs:289-300`）：`contains("timeout")` → timeout 等。HTTP adapter **从不读
receipt status 字段**（`adapters/mod.rs:44-53`，2xx 即 Succeeded）。任何新 adapter /
contract 工作若不先替换掉 message-sniffing，会继承同样的脆弱性——typed errors + 结构化
connector response 契约应是 Phase 2 的第一个增量，而不是事后补丁。

## 推荐的首批增量顺序（每个独立 PR，需单独签字）

按"先打地基、再加能力"排序，每一步都让后续步更安全：

1. **M2a — Operation catalog + typed errors**（最小、最不侵入）。
   引入 `OperationSpec { name, argument_schema, risk: ReadOnly|Write }`，把两个裸
   字符串收敛成单一事实源；gateway allowlist / arg 校验引用 catalog。同时把
   `DispatchErrorCategory::from_error` 的子串匹配换成 typed adapter errors
   （`thiserror` enum），并让 `HttpConnectorAdapter` 读响应 status 字段。
   - 不引入新能力，只去重 + 去脆弱。退出标准：现有行为不变，所有测试绿。

2. **M2b — Run principal execution profile**。
   引入 `ExecutionProfile`，按 channel + config 派生 grants（替代 gateway 硬编码）。
   配置驱动（`config.rs`），不引入新 operation。退出标准：cli/feishu 的 grant 集合可配。

3. **M2c — Fixed policy pipeline**。
   把 `approve_invocation` 的内联 3-clause 检查重构成有序 verdict pipeline
   （`PolicyVerdict::{Allow, Deny(reason), Transform(intent)}`）。仍是纯函数，无 I/O。
   退出标准：行为不变，pipeline 可扩展。

4. **M2d — Approval state（durable, opt-in）**。
   引入 `ApprovalRequest`/`ApprovalDecision` Journal fact + `AwaitingApproval` run
   status + resume path。**默认所有 operation 仍 inline-approve**（向后兼容）；只有
   catalog 标记为 `risk: Write` 的 operation 才进入 approval 流程。退出标准：write
   operation 暂停等待人工 decision，read-only 仍即时执行。

5. **M2e — 第一个低风险本地 adapter**。
   实现一个真正的 read-only 本地 adapter（如 `fs.read_file` 或 `time.now`），走完整
   intent → schema → policy → adapter → receipt 链路，验证 Phase 2 契约端到端可用。

## 不在 Phase 2 做的

- Feishu 入口直接获得 shell 权限；
- 模型临时生成未注册工具（operation 必须在 catalog 里）；
- 部署类 adapter；
- 通用 plugin registry / dynamic hook（roadmap 明确推迟到 Phase 3+）。

## Kernel 边界检查（standing goal）

每一步都要过 Kernel Thinness Gate：
- operation catalog / typed errors / execution profile / policy pipeline / approval
  state ——这些都是 protocol/state semantics（intent 是否被允许、是否需要 approval、
  receipt 可信），属于 Kernel。
- 真正的 adapter 实现（fs、http 工具）属于 Kernel 的 adapter trait 实现，不是 harness。
- 任何 eval / replay / dashboard 仍属于外部 harness，不进 `src/`。

## 何时实施

**不在本 session 实施。** 本文是 Phase 2 scoping，不是实现。每个 M2a–M2e 增量需
maintainer 单独签字，因为它们都改变 `src/` 的 protocol/state 语义。推荐从 M2a
（operation catalog + typed errors）开始——它不引入新能力，只去重 + 去脆弱，是后续
所有增量的安全地基。

## 参考

- `src/gateway/mod.rs:37-100`（validate/approve/recover，硬编码 principal）；
- `src/domain/mod.rs:79-104,223-265`（principal/grant/intent/receipt/error category）；
- `src/adapters/mod.rs:9-70,128-141`（adapter trait + HttpConnectorAdapter 不读 status）；
- `src/runtime/mod.rs:122,192,212-261`（同步 approval + create_run clone principal）；
- `docs/product-roadmap.md` Phase 2（目标与退出标准）。
