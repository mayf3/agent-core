# Decision: Ack/clear mechanism for terminal-unknown outbox rows

Follow-up to `docs/decisions/health-rollup-semantics.md` (档 C, PR #66). That
decision makes `/health.status` stay `degraded` while any terminal-unknown
outbox row exists (`outbox_unknown_count > 0`). This document analyzes how an
operator acknowledges / clears those rows so `degraded` is not permanent noise.
**Analysis only — no `src/` change. Awaiting maintainer sign-off before any
implementation.**

## 背景

PR #66 之前，`/health.status` 在恢复后回到 `ok`（因为 `unknown_invocations()`
排除了已经有 `OutboxDispatchUnknown` 的行），导致"有 terminal-unknown 但 status=ok"
的反直觉状态。档 C 修正了这点：terminal-unknown 现在让 `status` 保持 `degraded`。

副作用：一次 crash 之后，被恢复成 `unknown` 的 dispatch 会**长期**让系统停在
`degraded`，直到这些行被处理。因为：

- 恢复**故意**不自动重发 terminal-unknown（`recover_unknown_invocations` 的安全
  契约：never returns a row to `pending`）；
- outbox 的 terminal 转换有 `TERMINAL_TRANSITION_ERROR` 守卫，`unknown` 行不能被
  `succeed`/`fail`/`retry` 复用；
- `operating-guide.md` 现在写的是"如果你需要重发，那是人类的有意决策"——但仓库里
  **没有**任何机制支持这个决策。

所以需要一个明确的 ack/clear 路径。

## 当前状态（main）

- terminal-unknown 行：`outbox_dispatches.status = 'unknown'`，对应 Journal 里有
  `OutboxDispatchUnknown` terminal fact。
- `/health`：`outbox_unknown_count` 计这些行；档 C 起 `status` 因此 `degraded`。
- 无任何 API / CLI / SQL 模板让操作员 ack 或清除这些行。
- `operating-guide.md:144` 的 fault section 说重发是"out of scope for automatic
  recovery"，但没有给出操作员可执行的具体步骤。

## 需要回答的问题

1. "clear" 一个 terminal-unknown 行到底意味着什么？
   - (a) **acknowledge**：操作员确认"我知道这个 outcome 丢了，不再告警"，但不动
     任何状态——需要一个独立的 ack 标记让 `/health` 不再把它算进 degraded。
   - (b) **re-send**：操作员决定重新派发，产生一个**新的** outbox 行（不复用
     terminal 行），旧 terminal-unknown 行保留为历史。
   - (c) **purge**：直接从 `outbox_dispatches` 删除 terminal-unknown 行（危险，破坏
     projection 与 Journal 的一致性——Journal 里仍有 `OutboxDispatchUnknown` fact）。

2. 这个机制应该在哪里实现？
   - Kernel 内：一个 `/v1/admin/...` 端点或 CLI 子命令（**会**改变 Kernel 边界——
     目前 Kernel 没有任何 admin/mutation API，只有 ingress + health 读）。
   - 外部 harness：一个读 SQLite 的独立脚本（符合"audit/replay/ops 属于外部 harness"
     的边界原则）。

## 候选方案

### 方案 1：ack 标记（最小，纯读侧）

给 `outbox_dispatches` 加一列 `acked_unknown`（默认 0）。`/health` 的
`outbox_unknown_count`（及档 C rollup）改为只计 `status='unknown' AND
acked_unknown=0` 的行。操作员通过一个**外部脚本**（不是 Kernel API）直接
`UPDATE outbox_dispatches SET acked_unknown=1 WHERE ...` 标记已确认。

- 优点：最小改动；不动 Journal；不重发；projection 与 Journal 一致性不变；ack 是
  可逆的（`acked_unknown=0` 恢复告警）。
- 缺点：引入一列 + migration；ack 是"我放弃了"，语义消极；操作员仍需另外决定是否
  真的重发。ack 脚本如果写错（误 ack 非 unknown 行）会静默吞掉告警。

### 方案 2：re-send 产生新行（语义最干净）

操作员决定重发时，外部脚本读出 terminal-unknown 行的原始 invocation 信息，向 Kernel
的 `/v1/ingress`（或一个新的 re-dispatch 入口）**重新提交**，产生一条**新的** run +
outbox 行。旧的 terminal-unknown 行**保留**在历史里（Journal append-only 不变），但
`/health` 的 degraded 判定需要决定：旧 terminal-unknown 行是否继续算 degraded？

- 如果继续算 → 仍需要方案 1 的 ack 标记来让旧行退出 degraded。
- 如果重发后旧行自动退出 degraded → 需要一个"已被新 run 接替"的关联，复杂度高。

所以方案 2 通常需要**配合**方案 1。

### 方案 3：外部只读 + 文档化手动 SQL（不写代码）

不实现任何机制，只在 `operating-guide.md` 给出一段明确的 SQL 模板，让操作员自行
`UPDATE outbox_dispatches SET status='acked' ...`（引入一个非枚举的 acked 字符串状态），
并同步更新档 C rollup 的语义说明。

- 优点：零 `src/` 改动；保持 Kernel 无 admin API 的边界。
- 缺点：操作员直接改 production SQLite，容易写错；`acked` 是个 ad-hoc 状态，projection
  drift 检测（`outbox_projection_drift_count`，只认 `succeeded`/`failed`/`unknown`）
  可能把 `acked` 行算成 drift——需要同步改 drift 查询。

## 推荐

**方案 1（ack 标记），实现放在外部 harness，Kernel 只暴露读侧。**

理由：

1. 档 C 的 `degraded` 语义是对的（terminal-unknown 是真实运维债务），不应为了消音
   而削弱它；ack 标记是"操作员明确确认已知"的诚实表达。
2. Kernel 边界原则（standing goal + Architecture RFC）：admin/mutation API 不属于
   Phase 1 Kernel。ack 是对 projection 的写操作，应作为**外部 harness 脚本**实现
   （读 SQLite，`UPDATE ... SET acked_unknown=1`），Kernel 只需：
   - 加 `acked_unknown` 列（migration，schema 层面，不算 admin API）；
   - `/health` 的 `outbox_unknown_count` 与档 C rollup 排除 `acked_unknown=1` 的行。
3. re-send（方案 2）是独立的、更大的工作（需要重新走 ingress/gateway/runtime），
   不在本决策范围；ack 不阻塞未来 re-send。
4. purge（方案 c）禁止——破坏 projection/Journal 一致性。

### 实施清单（签字后）

1. `src/journal/queue.rs` migration：`ALTER TABLE outbox_dispatches ADD COLUMN
   acked_unknown INTEGER NOT NULL DEFAULT 0`（参照 `decision_id` 列的
   `ensure_*_column` 幂等模式）。
2. `src/journal/queue_health.rs`：`outbox_unknown_count` 查询加
   `AND acked_unknown = 0`；确认档 C rollup（用同一个 count）自动跟着变。
3. **不**在 Kernel 加 admin 端点。提供一段外部 SQL 模板写入
   `docs/operating-guide.md` 的 `outbox_unknown_count > 0` fault section
   （`UPDATE outbox_dispatches SET acked_unknown=1 WHERE invocation_id=?`），并说明
   ack 是可逆的（`=0` 恢复告警）。
4. 回归测试：`acked_unknown=1` 的行不计入 `outbox_unknown_count`；`/health.status`
   回到 `ok`。
5. 本文件标为已实施。

## 何时实施

**不是当前最小增量。** 它引入一列 schema migration + 改 health 查询 + 写操作员
文档 + 测试，并且触及"Kernel 是否暴露任何 mutation/admin 能力"的边界（即便 ack 走
外部脚本，schema 列仍由 Kernel 拥有）。等 maintainer 对本文件签字后再做。

## 参考

- `docs/decisions/health-rollup-semantics.md`（档 C，让 terminal-unknown 进入
  degraded——本文件是其 follow-up）；
- `src/server/mod.rs:190`（档 C rollup）；
- `src/journal/queue_health.rs`（`outbox_unknown_count` 查询）；
- `src/journal/queue.rs:36`（`outbox_dispatches` schema）+ `ensure_outbox_decision_id_column`
  （幂等 migration 模式参考）；
- `src/journal/outbox_queue.rs:9`（`TERMINAL_TRANSITION_ERROR`——为什么 terminal 行
  不能被复用）；
- `docs/operating-guide.md:144`（当前 terminal-unknown fault section）。
