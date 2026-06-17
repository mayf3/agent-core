# Decision: Phase 2 M2d — Durable approval state (opt-in)

Phase 2 Invocation Gateway Hardening 的最后一个增量。本文记录 M2d 的范围、设计取舍与
显式推迟项。参考 `docs/decisions/phase2-invocation-gateway-scoping.md`。

## 目标与退出标准

让 `risk: Write` 操作可暂停等待人工决策；`ReadOnly` 操作仍即时执行。**默认所有操作仍
inline-approve（向后兼容）；只有 operator 显式 opt-in，且 operation 在 catalog 中标记为
`risk: Write` 时，才进入 approval 流程。** 退出标准已达成（PR）。

## 设计：journal facts + in-process resume，opt-in config

暂停是**完全持久化**的（journal facts + run status，跨重启存活）。恢复在本增量是
**进程内** API（`Gateway::approve_run` / `Gateway::deny_run`），不新增 HTTP 端点——端点
是独立、可后续 review 的 follow-up，它增加 surface 但不改变持久化协议。不新增表：暂停
的 run 只 journal、不入 outbox；approve 后走**既有** outbox 路径，零改动。

被否决的方案（新表 + HTTP `/v1/approve` + 恢复扫描 + `/health` 降级 同 PR）：太大，会
危及 `tests/m5_parse_kind.rs` / `tests/m1_restart_recovery_lifecycle.rs` 守护的不变量。

## Opt-in gate

`KernelConfig.require_write_approval: bool`（env `AGENT_CORE_REQUIRE_WRITE_APPROVAL`，默认
`false`）。为 `false` 时 `Runtime::deliver` 行为与 M2d 前逐字节一致。

## 实现

- **domain** (`src/domain/mod.rs`)：`RunStatus::AwaitingApproval`；journal kinds
  `ApprovalRequested` / `ApprovalGranted` / `ApprovalDenied`。
- **parse_kind** (`src/journal/sqlite_read.rs`)：3 个新 arm。**关键**——缺它们则新 kind
  静默路由到 `Unknown` 哨兵并破坏 hash chain（正是 `tests/m5_parse_kind.rs` 防范的）。
- **Runtime** (`src/runtime/mod.rs`)：私有 `enqueue_or_pause`，在 `InvocationApproved`
  之后按 `operation::lookup(op).risk` + `config.require_write_approval` 分叉：
  - ReadOnly，或 Write 且未 opt-in → 既有路径（queue + `WaitingDispatch`）。
  - Write 且 opt-in → append `ApprovalRequested`（payload 含完整 `intent_snapshot`）+
    `update_run_status("AwaitingApproval")`，**不入队**。
  - `deliver` 与 `deliver_echo` 共用此 helper。
- **Gateway** (`src/gateway/mod.rs`)：`approve_run`（加载 snapshot → `ApprovalGranted` →
  queue → `WaitingDispatch`）/ `deny_run`（`ApprovalDenied` → `fail_run`）。两者幂等：run
  非 `AwaitingApproval` 时 no-op `Ok(())`。
- **JournalStore** (`src/journal/recovery.rs`)：`approval_request_for_run(run_id)` 扫描该
  run 的 `ApprovalRequested` fact。
- **/health** (`src/server/mod.rs`)：新增 `awaiting_approval_count`。**不**因它降级 rollup
  ——暂停是预期的 operator 状态，非信任损失（镜像既有"stale count 排除"的口径）。
- **recovery.rs delivered-set**：**无需改动**。delivered-set 以 `SessionReady` /
  `RunStarted`（在 proposal window 之前 append）为键，因此暂停的 run 已被计为已投递，
  重启不会重投。

## 测试 (`tests/m2d_approval_state.rs`, 8 个)

1. opt-out 时 Write 仍 inline-approve + queue（回归守护）。
2. opt-in 时 Write 暂停：`ApprovalRequested` 已 journal、run `AwaitingApproval`、未入队。
3. `approve_run` 恢复 → `ApprovalGranted` + `OutboxQueued` + `WaitingDispatch`。
4. `deny_run` 失败 → `ApprovalDenied` + `Failed`。
5. 幂等：对非 `AwaitingApproval` 的 run，approve/deny 是 no-op Ok。
6. approve 后再 deny 不会重复转移（幂等守护）。
7. 跨"重启"持久化：文件 DB 暂停 → 重开 → 仍 `AwaitingApproval`，fact 读回正确，且仍可
   恢复。
8. `parse_kind` round-trip：直接 append `ApprovalRequested` → 重开 → 读回为
   `ApprovalRequested`（非 `Unknown`），守护 hash-chain 不变量。

## Kernel Thinness Gate

approval/decision 是 protocol/state 语义（此 intent 现在 是否被允许执行）→ 属于 Kernel。
进程内 resume API 是 kernel state 机制。无 eval/replay/dashboard 代码，无新外部依赖。

## 显式推迟（residual risks）

- **无 HTTP `/v1/approve` 端点**（本增量仅进程内 API）→ follow-up PR。
- **无 approval 过期 / auto-stale**：暂停的 run 会一直等待直到被决策 → follow-up。
- Runtime 当前只 propose 回复类操作；把 Write tool-call 从 LLM 暴露出来是独立的
  context/tool-schema 增量。

## 参考

- `src/runtime/mod.rs`（`enqueue_or_pause`、`deliver` / `deliver_echo` 分叉点）；
- `src/gateway/mod.rs`（`approve_run` / `deny_run`）；
- `src/journal/recovery.rs`（`approval_request_for_run`、delivered-set 不变性）；
- `src/journal/sqlite_read.rs`（`parse_kind` 3 arm）；
- `tests/m2d_approval_state.rs`（8 个端到端 + 持久化测试）。
