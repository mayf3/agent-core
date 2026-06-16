# Decision: Should we introduce `RunStatus::Unknown`?

Phase 1 内核补强最后一项 open 决策。本文分析引入显式 `RunStatus::Unknown`
的影响,给出推荐,供 maintainer 拍板。

## 背景

当前一个 dispatch 的 outcome 不确定时(outbox projection 是 `unknown`),
对应的 run 状态停留在 `WaitingDispatch`(因为 `Runtime::deliver` 在 queue
outbox 后就把 run 设成 `WaitingDispatch`,之后只有 `succeed/fail` 才会改它)。
`unknown` 只存在于 outbox projection + health,不在 `runs.status` 上表达。

HANDOVER §10 / product-roadmap.md 把"是否引入 `RunStatus::Unknown`"列为
待决项,理由是 `WaitingDispatch` 语义上表示"等待派发",而 `unknown` 表示
"派发过但结果未知"——两者是不同的运维含义。

## 当前状态(main)

- `RunStatus` 枚举:`Running` / `WaitingDispatch` / `Completed` / `Failed`。
- `runs.status` 存储为**原始字符串**(`update_run_status(&str)`),不是
  `format!("{:?}", status)`。
- `run_status()` 返回 `Option<String>`——**没有任何代码把字符串 parse 回
  `RunStatus` 枚举**。
- recovery 把 `unknown` outbox 行设成 projection `unknown`,但**不改**
  `runs.status`(run 仍停在 `WaitingDispatch`)。
- `/health` 的 `unknown_invocation_count` / `unknown_invocations` 是从
  outbox projection 派生的,不从 `runs.status` 读。

## 影响面分析

### 序列化面(runs.status 列)

**零风险。** `runs.status` 是自由字符串。写入 `"Unknown"` 不需要 migration、
不需要改 schema、不需要改 `update_run_status` 签名。已有 DB 里的
`WaitingDispatch` 行不受影响。

### 写入面

引入 `Unknown` 需要在 recovery(`recover_unknown_invocations`)里,当 outbox
行被 reconcile 成 `unknown` 时,**同时**把对应 run 的 status 设成 `"Unknown"`。
这是一处新增 `update_run_status` 调用,机制清晰。

### 读取/match 面

**零风险。** 当前没有任何代码把 `runs.status` 字符串 parse 回 `RunStatus`
枚举做 exhaustive match。`run_status()` 返回 `Option<String>`,调用方只做
字符串比较或透传。新增一个 `"Unknown"` 字符串值不会破坏任何现有 match。

### DB 兼容

**零风险。** 已有 DB 里不会有 `"Unknown"` 行(老代码从不写它)。新代码只在
recovery 写它,且只针对已经被 reconcile 成 `unknown` 的 outbox 行对应的 run。
老 binary 读到 `"Unknown"` 字符串也只是当作未知 status 字符串透传(不会崩、
不会误判)。

### 运维语义

**这是真正的好处。** 引入后:
- `runs.status = "Unknown"` 明确表达"这个 run 的派发结果未知",区别于
  `WaitingDispatch`(还没派发)。
- 操作员查 `runs` 表或未来的 `/runs/:id` 接口时,能直接看到 `Unknown`,
  而不是被 `WaitingDispatch` 误导成"还在等"。
- `/health` 的 `unknown_invocation_count` 可以改为也从 `runs.status` 派生,
  让 run 状态和 outbox projection 语义一致。

### 潜在成本

- `RunStatus` 枚举新增一个 variant。由于没有 exhaustive match,不会强制
  任何调用点加 arm;但若未来有代码做 exhaustive match,需要处理 `Unknown`。
- recovery 多一次 `update_run_status` 调用(每行 unknown 一次)。可接受。
- 需要一个测试:recovery 把 outbox reconcile 成 unknown 时,run status 也
  变成 `Unknown`。

## 推荐方案

**引入 `RunStatus::Unknown`,但保持最小改动:**

1. `RunStatus` 枚举新增 `Unknown` variant。
2. recovery(`recover_unknown_invocations`)在把 outbox 行设成 `unknown` 的
   同时,调用 `update_run_status(run_id, "Unknown")`。
3. `succeed` / `fail` 路径不变(它们已经把 run 设成 `Completed`/`Failed`)。
4. 不改 `/health` 派生逻辑(`unknown_invocation_count` 继续从 outbox projection
   派生,保持单一事实源;`runs.status = "Unknown"` 是补充信号,不是替代)。

**理由:** 写入面/读取面/DB 兼容都是零风险(已分析),真正的成本只是一个枚举
variant + 一处 recovery 调用 + 一个测试,而收益是运维语义清晰。这是 Phase 1
内核补强的自然收口。

## 不推荐的替代方案

- **不引入,保持 `WaitingDispatch`:** 当前状态。缺点是语义混淆(unknown ≠
  waiting),操作员查 run 状态会被误导。可接受但不够干净。
- **引入并把 `/health` 改成从 `runs.status` 派生 unknown:** 更大改动,引入
  双事实源风险(outbox projection vs runs.status 可能短暂不一致)。不推荐
  在 Phase 1 做。

## 决策状态

**待 maintainer 拍板。** 若同意推荐方案,实现是一个小 PR(枚举 variant +
recovery 调用 + 测试)。
