# M5 Outbox Stabilization — Plan

## Background

PR1-5 已完成状态机契约 / dispatch failure policy / retry / Runtime 不再同步发送 / dispatcher loop 接入。本分支基于 PR5 dirty tree 继续，目标是把 dispatcher 接入后的状态语义收口，让 Run / Outbox / Journal 三者在 success / failure / unknown 三种结局下自洽。

## Current Reality (verified by code reading)

主链路：

```
/v1/ingress accepted (accept_ingress_with_worker_job)
-> worker_jobs queue
-> worker loop (lease_next_worker_job -> process_next_worker_job)
-> Runtime::deliver
   -> Session, Run, Context, LLM
   -> reply_intent -> gateway.approve_invocation
   -> journal.queue_outbox_dispatch (OutboxQueued + projection pending)
   -> journal.update_run_status("WaitingDispatch")
-> outbox dispatcher loop (start_outbox_dispatcher_loop)
-> dispatch_once
   -> journal.lease_next_outbox_dispatch (DispatchStarted + projection dispatching)
   -> adapter.execute (HTTP -> connector /v1/execute)
   -> journal.succeed_outbox_dispatch | fail_outbox_dispatch | unknown_outbox_dispatch
```

## Gaps (verified)

1. `succeed_outbox_dispatch` (`src/journal/outbox_queue.rs:124`) 写 `ReceiptReceived` + 改 projection，**没有**改 `runs.status`，**没有**写 `RunCompleted`。Run 永远停在 `WaitingDispatch`。
2. `fail_outbox_dispatch` (`src/journal/outbox_queue.rs:162`) 写 `ReceiptReceived(status=Failed)` + 改 projection，**没有**改 `runs.status`，**没有**写 `RunFailed`（`JournalEventKind::RunFailed` 不存在）。
3. `unknown_outbox_dispatch` (`src/journal/outbox_queue.rs:203`) 写 `OutboxDispatchUnknown` + 改 projection，符合任务 3 期望，无需修改逻辑。
4. `recover_unknown_invocations` (`src/journal/unknown.rs:44`) 当前错误地：
   - 写 `ReceiptReceived(status=Unknown)` （违反任务 4.1）
   - 调 `fail_run` 并写 `RunCompleted(reason=unknown_invocation_recovered)` （违反任务 3：unknown 不 complete）
   - 不满足"OutboxDispatchUnknown 是 terminal fact"的语义
5. `unknown_invocations()` (`src/journal/unknown.rs:9`) 的 SQL 已经过滤掉 `ReceiptReceived` 和 `OutboxDispatchUnknown`，符合任务 4.1 候选定义，**保留**。
6. `health_snapshot` (`src/server/mod.rs:144`) 缺 `outbox_dispatcher_enabled / outbox_pending_count / outbox_unknown_count / outbox_dispatching_count` 字段。
7. `serve()` (`src/server/mod.rs:18`) 在启动 dispatcher 前没有任何脱敏提示日志。
8. `src/server/delivery.rs:157` 测试模块 `use rusqlite::params;` 实际未使用（测试里全部用 fully-qualified `rusqlite::params!`）。
9. `docs/architecture/outbox.md` 仍写 "Runtime still synchronous / dispatch_once not connected to server startup"，已过期。
10. （不在本任务范围但记录）`parse_kind` 在 `src/journal/sqlite.rs:417` 的 `_ => JournalEventKind::RunCompleted` fallback 是 pre-existing bug，本次只补 `RunFailed` 分支，不动 fallback。

## Solution Design

### Task 1 — Success path completes Run

修改 `JournalStore::succeed_outbox_dispatch` (`src/journal/outbox_queue.rs`)，在现有 `BEGIN IMMEDIATE` 事务内增加：

```text
UPDATE runs SET status='Completed', updated_at=? WHERE id=?
append_event_tx(RunCompleted, run_id, session_id, Some(receipt.invocation_id), {status: Succeeded})
```

不变量：
- 一个 tx 同时落 projection / ReceiptReceived / runs.status / RunCompleted
- 不破坏现有 `outbox_dispatch_lifecycle_updates_projection_and_journal` 测试（run_id 不在 runs 表里时 UPDATE 0 行，无副作用）

### Task 2 — Definite failure fails Run

1. 在 `src/domain/mod.rs` 的 `JournalEventKind` 增加 `RunFailed` 变体。
2. 在 `src/journal/sqlite.rs::parse_kind` 增加 `"RunFailed" => JournalEventKind::RunFailed` 分支。
3. 修改 `JournalStore::fail_outbox_dispatch` 在现有 tx 内增加：

```text
UPDATE runs SET status='Failed', updated_at=? WHERE id=?
append_event_tx(RunFailed, run_id, session_id, Some(invocation_id), {status: Failed, error})
```

### Task 3 — Unknown path

`unknown_outbox_dispatch` 已经不写 `RunCompleted`，**不改代码**。Run 保持 `WaitingDispatch`，由 outbox projection `unknown` + health 字段暴露。文档需在 `docs/architecture/outbox.md` 明确：unknown outbox 对应的 Run **不是** completed。

### Task 4 — DispatchStarted crash recovery

重写 `JournalStore::recover_unknown_invocations` (`src/journal/unknown.rs`)：

```text
Step A: candidates = unknown_invocations()   // SQL 已正确，过滤 ReceiptReceived + OutboxDispatchUnknown
        for each candidate:
          tx {
            append_event_tx(OutboxDispatchUnknown, run_id, session_id, Some(invocation_id), {recovered: true, ...})
            UPDATE outbox_dispatches SET status='unknown', locked_by=NULL, locked_until=NULL, updated_at=? WHERE invocation_id=?
          }

Step B: stale_dispatching_with_terminal_journal()  // 新加 SQL 查询
        SELECT od.invocation_id FROM outbox_dispatches od
        WHERE od.status='dispatching' AND (od.locked_until IS NULL OR od.locked_until <= ?now)
          AND EXISTS (
            SELECT 1 FROM journal_events je
            WHERE je.correlation_id = od.invocation_id
              AND je.kind IN ('OutboxDispatchUnknown', 'ReceiptReceived')
          )
        for each invocation_id:
          tx {
            UPDATE outbox_dispatches SET status='unknown', locked_by=NULL, locked_until=NULL, updated_at=? WHERE invocation_id=?
          }
          // 不写 journal event，journal 已经是 terminal
```

明确删除：
- 不再写 `ReceiptReceived(status=Unknown)`
- 不再调 `fail_run`
- 不再写 `RunCompleted`
- 不调 adapter，不调 `dispatch_once`

### Task 5 — Dispatcher enabled=true safety hints

1. 在 `src/journal/queue_health.rs` 新增：

```rust
pub fn outbox_status_count(&self, status: OutboxDispatchStatus) -> Result<i64>
```

2. 修改 `health_snapshot` 签名：

```rust
pub fn health_snapshot(journal: &JournalStore, outbox_dispatcher_enabled: bool) -> Result<Value>
```

新字段：

```json
{
  "outbox_dispatcher_enabled": <bool>,
  "outbox_pending_count": <i64>,
  "outbox_unknown_count": <i64>,
  "outbox_dispatching_count": <i64>
}
```

3. 在 `serve()` 启动 dispatcher 前（无论 enabled true/false）打印：

```text
outbox_dispatcher_enabled={true|false}
existing_pending_outbox_count={N}
existing_unknown_outbox_count={N}
existing_dispatching_outbox_count={N}
dispatcher will process pending/retryable outbox items
unknown items will not be retried automatically
```

禁止打印：arguments_json / 用户文本 / 飞书 payload / token / secret / 完整 HTTP response。

4. 更新 `handle_connection` 中 `/health` 调用为 `health_snapshot(&journal, config.outbox_dispatcher_enabled)?`。

### Task 6 — Warning & doc cleanup

1. 删除 `src/server/delivery.rs` 测试模块里 `use rusqlite::params;`。
2. 更新 `docs/architecture/outbox.md`：
   - 移除/纠正 "Runtime still synchronous"
   - 移除/纠正 "dispatch_once not connected to server startup"
   - 增加 "Run status on dispatch outcome" 章节（success→Completed, failure→Failed, unknown→保持 WaitingDispatch）
   - 增加 "OutboxDispatchUnknown 是 terminal fact" 章节
3. `design-doc.html` 不动，只在 `docs/m5-outbox-stabilization/` 留 TODO 标注其 stale。

## Deliverables (files to change)

新增：
- `docs/m5-outbox-stabilization/plan.md`
- `docs/m5-outbox-stabilization/todo.md`
- `docs/m5-outbox-stabilization/validation_layout.py`

修改：
- `src/domain/mod.rs`（新增 `RunFailed` 事件 kind）
- `src/journal/sqlite.rs`（`parse_kind` 增加 `RunFailed` 分支）
- `src/journal/outbox_queue.rs`（`succeed_outbox_dispatch` 同 tx 写 `RunCompleted`；`fail_outbox_dispatch` 同 tx 写 `RunFailed`）
- `src/journal/unknown.rs`（重写 `recover_unknown_invocations`；新增 `stale_dispatching_with_terminal_journal`）
- `src/journal/queue_health.rs`（新增 `outbox_status_count`）
- `src/server/mod.rs`（`health_snapshot` 加 `outbox_dispatcher_enabled` 参数 + 新字段；`serve` 加启动日志）
- `src/server/delivery.rs`（删 unused `use rusqlite::params;`）
- `docs/architecture/outbox.md`（更新当前状态描述）

测试更新：
- `tests/m0_kernel.rs`（`health_snapshot` 新签名；`journal_recovery_marks_unknown_invocations` 改断言 `OutboxDispatchUnknown` 而非 `ReceiptReceived(status=Unknown)`）
- `tests/m5_queue_projection.rs`（`health_snapshot` 新签名；`unknown_recovery_*` 改断言；新增 success completes run / failure fails run / unknown not completes / no duplicate RunCompleted / stale dispatching never returns to pending / existing OutboxDispatchUnknown stops scan / dispatcher disabled / health fields）

不修改：
- `connectors/feishu/`（任何 TS 文件）
- `src/runtime/mod.rs`（Runtime 不重新引入同步路径）
- `src/runtime/outbox_dispatcher.rs::dispatch_once`（PR5 已实现，调用 journal helpers 即可）
- `Cargo.toml`、`package.json`

## Non-Goals (per task spec)

- 不恢复 Runtime 同步发送旁路
- 不修改 TS Feishu Connector
- 不实现 connector-local reaction retry
- 不实现 projection rebuild / repair
- 不实现 deploy / restart 脚本
- 不自动重发 unknown outbox
- 不把 stale dispatching 改回 pending / retryable_failed
- 不读 `.env` / `~/.openduck` / `~/.openclaw` / 密钥文件 / 敏感日志
- 不大改 Run 状态机（unknown 用 WaitingDispatch + health 暴露）

## Safety invariants

- ReceiptReceived 只在 success 和 definite failure 路径写
- OutboxDispatchUnknown 是 unknown 唯一 terminal fact
- DispatchStarted 一旦写入，无 ReceiptReceived 时禁止自动重试
- succeed_outbox_dispatch / fail_outbox_dispatch 单一事务包含 projection + journal + runs
- 启动 recovery 不调 adapter
