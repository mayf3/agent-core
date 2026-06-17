# Decision: Should `/health.status` degrade on `undelivered_ingress_count`?

Follow-up to `docs/decisions/health-rollup-semantics.md` (档 C, PR #66). That
decision intentionally excluded `undelivered_ingress_count`, flagging it as a
separate evaluation. This document records that evaluation and the decision.
Maintainer approved including it this session.

## 背景

`undelivered_ingress_count` 来自 `journal.undelivered_ingress_events().len()`
（`src/journal/recovery.rs:18`）。它表示：**已被 Gateway accept、写入 Journal 的
ingress 事件，但还没有对应的 `SessionReady` / `RunStarted` / `RunCompleted` /
`RunFailed` 关联**——也就是"消息进来了，但从来没变成 worker job / run"。

这和 terminal-unknown（PR #66 已纳入 degraded）不同：
- terminal-unknown = 派发过，结果未知；
- undelivered = 连派发都没发生（worker loop 没消费 / 启动恢复没把它重新入队）。

从运维角度看，undelivered 比 unknown 更严重：用户的消息被吞了，没有任何 run 产生。
档 C 把 unknown 纳入 degraded 却把 undelivered 留在 `ok`，是反直觉的。

## 影响面分析（已在本次核实）

### 现有测试

档 C 决策文档当时写"会改变现有测试期望"。本次核实：**实际不会**。

- `tests/ingress_recovery.rs:69 health_reports_undelivered_ingress_count` 只断言
  `undelivered_ingress_count` 的值，**不**断言 `status`。
- 全仓唯一一处 `status == "ok"` 断言是 `tests/m1_projection_drift.rs:156
  steady_state_health_is_ok`，用的是空 Journal（`undelivered_ingress_count == 0`）。
- `tests/m0_kernel.rs:201` 断言 `status == "degraded"`（有 unknown invocation），
  本来就 degraded，不受影响。
- `tests/m1_restart_recovery_lifecycle.rs:48` 断言恢复前 `status == "degraded"`
  （abandoned dispatch → live unknown），本来就 degraded，不受影响。

所以把 `undelivered_ingress_count > 0` 纳入 degraded **不会破坏任何现有测试**。

### 运维语义

`status` 在以下任一情况为 `degraded`（档 C + 本扩展）：

1. live unknown invocations；
2. terminal-unknown outbox rows（`outbox_unknown_count > 0`）；
3. projection drift（`outbox_projection_drift_count > 0`）；
4. **undelivered ingress（`undelivered_ingress_count > 0`）—— 本次新增。**

stale counts 仍然排除（自愈瞬态）。

### 启动期行为

正常启动时 `recover_undelivered_ingress`（`src/server/delivery.rs:42`，由
`serve()` 调用）会把 undelivered ingress 重新入队为 worker job，之后
`undelivered_ingress_events()` 会变空。所以 degraded 是**瞬态**：恢复完成后
`status` 应回到 `ok`。如果恢复后仍 `undelivered > 0`，说明 worker job 入队失败或
Journal 损坏，degraded 是正确的信号。

## 决策

**纳入 degraded**（maintainer 已签字）。在 `health_snapshot` 的 degraded 谓词里
增加 `|| undelivered_ingress_count > 0`。

### 实施清单

1. `src/server/mod.rs`：rollup 谓词增加 `undelivered_ingress_count > 0`，更新注释。
2. `docs/operating-guide.md`：更新 `degraded` 定义，包含 undelivered ingress。
3. 回归测试：undelivered ingress > 0 时 `status == "degraded"`；入队（恢复）后
   回到 `ok`（验证瞬态语义）。
4. 本文件标为已实施。

## 参考

- `src/server/mod.rs:190`（rollup，PR #66 后的状态）；
- `src/journal/recovery.rs:18`（`undelivered_ingress_events`）；
- `src/server/delivery.rs:42`（`recover_undelivered_ingress`，启动期清空）；
- `docs/decisions/health-rollup-semantics.md`（档 C 母决策）。
