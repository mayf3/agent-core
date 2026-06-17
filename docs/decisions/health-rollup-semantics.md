# Decision: `/health.status` rollup 应该把哪些计数算作 degraded？

Phase 1 Operational Hardening 的一项 open 决策。当前 `/health.status` 的 rollup
只看 hash chain 和 live unknown invocations，本文分析是否应把 terminal-unknown /
projection drift / stale worker 也纳入 degraded，并给出推荐，供 maintainer 拍板。
本文件只做分析与决策，不改动任何 `src/`。

## 背景

`/health` 暴露很多计数（见 `docs/operating-guide.md` 的 health 表），但顶层
`status` rollup 的判定逻辑很窄。运维场景下经常遇到：某个计数 > 0（比如
`outbox_unknown_count`），但 `status` 仍是 `ok`，让人困惑"这到底算不算健康"。
在 productize 之前需要明确 rollup 语义。

## 当前状态（main）

`src/server/mod.rs:190-196` 的 rollup：

```rust
let status = if !hash_chain_ok {
    "corrupt"
} else if unknown_invocations.is_empty() {
    "ok"
} else {
    "degraded"
};
```

其中 `unknown_invocations` 来自 `journal.unknown_invocations()`
（`src/journal/unknown.rs:23-56`）。关键点：这个查询的 WHERE 子句里有

```sql
AND NOT EXISTS (
  SELECT 1 FROM journal_events u
  WHERE u.kind = 'OutboxDispatchUnknown'
    AND u.correlation_id = d.correlation_id
)
```

也就是说：**一旦某个 dispatch 被 recovery 写了 `OutboxDispatchUnknown` terminal
fact，它就不再出现在 `unknown_invocations()` 里**。

后果：一次 crash 之后，启动 recovery 把所有无 receipt 的 dispatch 标成
`OutboxDispatchUnknown`；此后 `unknown_invocations()` 返回空 → `status` 从
`degraded` 翻回 `ok`。但同时：

- `outbox_unknown_count`（projection 里 `status = 'unknown'` 的行数）可能仍 > 0；
- `outbox_projection_drift_count` 在 recovery 成功后应回到 0，但若 recovery 没跑
  或有 race，可能 > 0；
- `worker_job_stale_count` / `outbox_stale_dispatching_count` 在正常稳态是 0，
  crash 后到下一次 lease 之前可能短暂 > 0。

`docs/operating-guide.md` 当前把语义写成：

- `ok` — hash chain intact and no unknown invocations；
- `degraded` — hash chain intact but unknown invocations present (recoverable)；
- `corrupt` — hash chain broken。

所以"status=ok 但 outbox_unknown_count>0"是文档化逻辑的直接结果，不是 bug——
但它对运维者反直觉。

## 需要回答的问题

1. terminal-unknown dispatch（已经被 recovery 定性为 `unknown`，不会自动重发）
   算不算 degraded？
2. projection drift（recovery 没把 projection 修对）算不算 degraded？
3. stale worker / stale dispatching（lease 过期、等下一轮 reclaim）算不算
   degraded？还是只算瞬态？

## 候选语义（三档）

### 档 A：保持现状（status 只反映 live unknown + hash chain）

- `status` = `corrupt` | `degraded`(有 live unknown) | `ok`。
- terminal-unknown、drift、stale 都通过各自计数暴露，**不**影响 rollup。
- 优点：语义稳定，`status` 反映"有没有需要人工立即处理的活体异常"。
- 缺点：`status=ok` 与 `outbox_unknown_count>0` 并存，对运维者反直觉，需要文档
  解释清楚。

### 档 B：把 terminal-unknown 也算 degraded（最小扩展）

rollup 扩展为：

```rust
let status = if !hash_chain_ok {
    "corrupt"
} else if !unknown_invocations.is_empty() || outbox_unknown_count > 0 {
    "degraded"
} else {
    "ok"
};
```

- 含义：只要还有任何 unknown（live 或 terminal）就 degraded。
- 优点：`status` 与 `outbox_unknown_count` 一致，运维者一眼能看出"有事"。
- 缺点：crash 恢复后 `status` 会**长期**停在 `degraded`，直到人工清理或重发
  terminal-unknown 行；可能让"已恢复但留有历史 unknown"的系统长期亮黄。需要配合
  一个"ack / clear terminal unknown"的操作手段，否则 degraded 会变成噪音。

### 档 C：把 drift 也算 degraded（projection 不一致 = 不健康）

rollup 进一步：

```rust
} else if !unknown_invocations.is_empty()
        || outbox_unknown_count > 0
        || outbox_projection_drift_count > 0 {
    "degraded"
}
```

- 含义：projection 与 Journal 不一致本身就是不健康（recovery 没修对 = bug）。
- 优点：`status` 真实反映"projection 是否可信"。drift 在稳态本应是 0，非 0 一定是
  recovery 失败或 race，值得告警。
- 缺点：stale（lease 过期）是否也算？stale 是**预期**会被下一轮 reclaim 自愈的瞬态，
  把它算 degraded 会在每次 crash 后短暂亮黄，可能制造告警风暴。建议 stale **不**
  纳入 rollup，只通过计数暴露。

## 推荐

**档 C，但 stale 计数不纳入 rollup**。理由：

1. `outbox_projection_drift_count > 0` 在稳态只能是 recovery 失败或 race，是真实的
   "projection 不可信"信号，应当让 `status` 反映它；
2. terminal-unknown（`outbox_unknown_count > 0`）代表"有 dispatch 永远不会自动
   完成"，是真实的运维债务，应当 degraded；
3. stale（`outbox_stale_dispatching_count` / `worker_job_stale_count`）是**自愈**
   瞬态（下一轮 lease 会 reclaim），纳入 rollup 会制造噪音。它们已经通过独立计数
   暴露，足够。

建议的 rollup（档 C）：

```rust
let status = if !hash_chain_ok {
    "corrupt"
} else if !unknown_invocations.is_empty()
        || outbox_unknown_count > 0
        || outbox_projection_drift_count > 0
{
    "degraded"
} else {
    "ok"
};
```

注意：`undelivered_ingress_count > 0`（有 accepted 但完全没派发过的 ingress）目前
**也不**影响 `status`。建议一并讨论是否纳入——它代表"有消息进了 Journal 但从没被
worker 接走"，比 unknown 更需要告警。倾向也纳入 degraded，但这会改变现有测试期望，
需要单独评估。

## 影响面（若按档 C 实施）

- `src/server/mod.rs:190-196`：rollup 表达式扩展（约 1 处）；
- `docs/operating-guide.md`：更新 `status` 取值说明（ok/degraded 的触发条件）；
- 现有测试：需检查 `tests/` 下断言 `status == "ok"` 的用例，在引入 unknown/drift
  数据后是否仍成立；可能需要新增"terminal unknown → degraded"的回归测试；
- 监控/告警：若已有外部系统按 `status` 告警，crash 恢复后告警会变频繁，需要同步
  调整告警阈值或提供 ack 手段；
- 不影响 Journal 事实、不影响 recovery 行为、不影响 dispatch 路径——纯 rollup 表达。

## 何时实施

不是当前最小增量。它**改变 `/health.status` 的对外语义**（运维和潜在外部监控依赖），
属于 productization 决策，需要 maintainer 对"terminal unknown 是否长期 degraded"
拍板（这关系到是否要配套 ack/clear 操作）。本文件只做分析与推荐。

## 参考

- `src/server/mod.rs:173-225`（`health_snapshot` rollup）；
- `src/journal/unknown.rs:23-56`（`unknown_invocations` 查询，排除 terminal unknown）；
- `src/journal/queue_health.rs:61-99`（`outbox_projection_drift_count`）；
- `docs/operating-guide.md` health 表与 `status` 取值说明；
- `docs/decisions/runstatus-unknown.md`、`docs/decisions/worker-failure-journal-kind.md`
  （同类决策文档格式参考）。
