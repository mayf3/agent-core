# Decision: Worker delivery failure 应写哪种 Journal terminal fact?

Phase 1 内核补强的一项 open 改进。当前 worker 投递失败时写的事实语义混乱，
本文分析改动的影响面并给出推荐，供 maintainer 拍板。本文件只做分析与决策，
不改动任何 `src/`。

## 背景

`src/server/delivery.rs` 的 `process_next_worker_job` 在 worker 投递失败时，
向 Journal 追加：

```rust
JournalEventKind::RunCompleted,   // ← 注意：用的是 RunCompleted
payload = { "status": "Failed", "reason": "worker_delivery_failed",
            "error_category": <category> }
```

也就是说：**一次失败的 worker 投递，在 run timeline 上被记录成一个
`RunCompleted` 事实**（只是 payload 里塞了 `status = Failed`）。这对今天的
"已投递标记"是有用的，但语义上很混乱：

- `RunCompleted` 字面意思是"run 完成"，而失败投递并没有完成 run；
- 同一个 kind 既表达成功完成，也表达失败投递，靠 payload 区分；
- 与 outbox 失败路径不一致（见下）。

这对未来的 replay / evaluation / run analytics 会造成歧义：读 timeline 时无法
只靠 `kind` 判断这条事实是"成功完成"还是"投递失败"。

## 当前状态（main）

两种 terminal fact 已存在且语义不同：

| 失败路径 | 写入的 kind | 写入的 payload.status | 位置 |
|---|---|---|---|
| **worker 投递失败** | `RunCompleted` | `"Failed"` | `src/server/delivery.rs:72-82` |
| **outbox 派发失败** | `RunFailed` | `"Failed"`（外层 `ReceiptReceived`） | `src/journal/outbox_queue.rs:212-230` |

即 outbox 派发失败已经走 `RunFailed`，而 worker 投递失败却走 `RunCompleted`。
两条失败路径在 Journal 上的 terminal fact 不一致。`JournalEventKind::RunFailed`
已经存在于枚举里（`src/domain/mod.rs:349`），只是 worker 投递失败这条路径
没用它。

## 关键约束：recovery 谓词依赖 `RunCompleted`

`src/journal/recovery.rs:18-45` 的 `undelivered_ingress_events()` 用如下逻辑判断
"哪些已 accept 的 ingress 还没被处理过"：

```text
delivered := { event.correlation_id
               for event in events
               if kind in {SessionReady, RunStarted, RunCompleted} }
undelivered := [ e for e in IngressAccepted if e.event_id not in delivered ]
```

注意：**`RunCompleted`（无论 payload.status 是 Completed 还是 Failed）都被视为
"已处理过"**。这恰恰是当前 worker 失败路径写 `RunCompleted` 的作用——它把这次
ingress 标记为"已处理"，从而**不会**在重启时被 `recover_undelivered_ingress`
重新入队。

`RunFailed` **不在**这个 delivered 集合里。

## 影响面分析

### 改动方案 A：把 worker 失败路径从 `RunCompleted` 改为 `RunFailed`

最小代码改动是 `src/server/delivery.rs:73` 把 `JournalEventKind::RunCompleted`
改成 `JournalEventKind::RunFailed`（payload 可保留或调整）。

**单独做这一步会引入 correctness regression**：

- `undelivered_ingress_events()` 不再把这次 ingress 视为已处理；
- 每次 kernel 重启，`recover_undelivered_ingress`（`src/server/delivery.rs:42`，
  由 `serve()` 启动调用）都会把这个**已经失败过**的 ingress 事件重新入队；
- worker loop 会再次尝试投递同一个 ingress → 永久重投，且每次都失败、每次都
  追加一条 `RunFailed`，Journal 膨胀。

这是为什么当前实现"故意"用 `RunCompleted`：它需要那个 delivered 标记。

### 改动方案 B：改 kind + 同步更新 recovery 谓词

要让方案 A 正确，必须同时改 `src/journal/recovery.rs:22-27`，把
`JournalEventKind::RunCompleted` 扩展为 `{RunCompleted, RunFailed}`：

```text
if matches!(event.kind,
    JournalEventKind::SessionReady
    | JournalEventKind::RunStarted
    | JournalEventKind::RunCompleted
    | JournalEventKind::RunFailed)   // ← 新增
```

这样失败的 worker 投递仍被视为"已处理"（不会重投），但 terminal fact 用语义
正确的 `RunFailed`。

方案 B 的影响面：

- `src/server/delivery.rs`：1 处 kind 改动；
- `src/journal/recovery.rs`：1 处谓词扩展；
- 现有测试 `tests/ingress_recovery.rs`：用 `SessionReady` / `RunStarted` 验证
  delivered 语义，不依赖 `RunFailed`，**应仍绿**；需补一个新测试锁定
  "`RunFailed` 也算 delivered"的不变式；
- `/health` 的 `undelivered_ingress_count`：不会因为本改动而上升（因为谓词同步
  扩展了），无行为变化；
- `verify_hash_chain`：kind 文本改变只影响**新写入**的行，旧库的旧 `RunCompleted`
  行读回来仍 decode 为 `RunCompleted`，向后兼容；
- 历史 SQLite 数据：已有的失败投递行仍是 `RunCompleted` + `status:Failed`，不会被
  回填改写；replay/eval 如果按 kind 区分成功/失败，需要同时认旧格式（payload
  `status:Failed` on `RunCompleted`）和新格式（`RunFailed`）。

### 不改动（方案 C）

保持现状，但在文档里明确"worker 投递失败写成 `RunCompleted` + `status:Failed`
是有意的 delivered 标记，不是 bug"。代价是长期语义混乱，未来 analytics 需要特殊
case 这条 payload。

## 推荐

**方案 B**，但前提是作为一次独立的小 PR，且：

1. 在同一 PR 里同时改 `src/server/delivery.rs`（kind）和
   `src/journal/recovery.rs`（谓词），二者必须原子提交，不能拆开（拆开会在中间
   状态产生重投 bug）；
2. 补一条回归测试：写一条 `RunFailed`（对应一个已 accept 的 ingress），断言
   `undelivered_ingress_events()` 不包含它（即"失败也算已处理，不重投"）；
3. 跑全套 `pnpm check` + `cargo test` + M5 `validation_layout.py`；
4. 在 `docs/operating-guide.md` 或 milestones 里记一句"worker 投递失败的 terminal
   fact 从 `RunCompleted`+Failed 改为 `RunFailed`，recovery 谓词同步扩展"，方便
   以后 replay/eval 作者知道新旧两种格式都存在。

不推荐方案 A（单独改 kind）——它本身就会引入重投 regression。
不推荐方案 C——长期语义成本高于一次性改对。

## 何时实施

不是当前最小增量。它**改动 durable Journal 事实 + recovery 谓词**，属于
Journal-semantics redesign，比 doc-only 改动风险高。等 maintainer 对本文件
签字后再做，且必须按上面 1-4 的顺序，kind 改动与谓词改动不能分两次 PR。

## 参考

- `src/server/delivery.rs:54-91`（worker 失败写入点）；
- `src/journal/recovery.rs:18-45`（`undelivered_ingress_events` 谓词）；
- `src/journal/outbox_queue.rs:183-230`（outbox 失败已用 `RunFailed`）；
- `src/domain/mod.rs:348-349`（`RunCompleted` / `RunFailed` 枚举）；
- `tests/ingress_recovery.rs`（delivered 语义测试）；
- `docs/decisions/runstatus-unknown.md`（同类决策文档的格式参考）。
