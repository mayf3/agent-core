# Decision: Connector-local durability before extraction

`docs/current-goal.md` 把"在 connector-local durability 更清晰之前不要把 Feishu
移出仓库"列为 open 项（Phase 3 plugin/connector surface 的前置条件）。本文基于对
`connectors/feishu/src` 和 Rust 侧 IPC 契约的实际代码核查，记录 connector 当前
durability 状态、识别真实缺口、给出推荐。**Analysis only — no `src/` /
`connectors/` change. 不在 Phase 3 之前实现任何 extraction。**

## 背景

Product roadmap（`docs/product-roadmap.md`）的 Phase 3 目标是"把通道和外部能力增长
从 Kernel 中移出去"，其中第一项是把 Feishu 变成独立 connector / plugin。但在那之前
要先把 connector-local durability 想清楚，否则把一个内存态的 connector 拆出去会
放大重复回复风险。

## 当前状态（main，已核查）

### 持久化的（survives connector restart）

只有一样东西：**reaction 状态**（`processing` / `failed`），以 append-only JSONL 持久化。

- `connectors/feishu/src/reaction-store.ts`：`StoredReactionState = { messageId,
  reactionId, emojiType, status: "processing"|"failed", updatedAt }`（line 12-18）。
- 文件路径：env `AGENT_CORE_FEISHU_REACTION_STATE_PATH`，默认
  `<data_dir>/feishu-reactions.jsonl`（`config.ts:66-71`）；`data_dir` 默认
  `~/.agent-core`（`config.ts:73-75`）。
- 启动加载：`createReactionTracker` → `loadStates` → `store.load()` →
  `loadJsonl` 按顺序重放 `set`/`delete`（`reaction-store.ts:81-99`）。
- 压缩（M1b）：`compactIfNeeded` 在每次写后按 `compactAfterBytes`（默认 256KB）触发，
  `compactJsonl` 重写为纯 `set` 记录并原子 `rename`（`reaction-store.ts:101-121`）。
- `withRetry`（`reactions.ts:166-191`）：bounded 指数退避 + full jitter，包住
  addReaction / removeReaction 两个 REST 调用；保留 Phase 0 "每条消息最多 N 次 add /
  N 次 delete"不变式。

### 不持久化的（lost on connector restart）

1. **Execute 幂等缓存** —— `execute-server.ts:6` 的 `const executions = new
   Map<string, Promise<unknown>>()`，key 是 `idempotency_key`（line 17），value 是
   in-flight 的 `sendReply` promise。**纯内存，无磁盘 backing。** 而且成功条目**永不
   淘汰**（只在 `.catch` 里 `delete`，line 31），Map 随进程生命周期无界增长。
2. **`remove_pending` 并发 guard** —— `reactions.ts:135`，纯进程内并发标记，故意不
   持久化（`reactions.ts:131-134, 150-156`）。
3. **WS 长连接会话** —— `index.ts:29-34`（Lark `WSClient`）；SDK 自带重连，但 crash
   时 in-flight 事件的交付语义由 Feishu 决定，不由 connector 决定。

### IPC 契约的幂等性不对称（关键缺口）

- **Ingress（connector → Kernel）**：去重在 **Kernel 侧**，基于 Journal 的
  `external_event_id`（= `message:<messageId>`，`kernel.ts:52-53`）；ingress envelope
  **没有** `idempotency_key` 字段。connector 重启不丢 ingress 去重。
- **Execute（Kernel → connector）**：去重在 **connector 侧**，且**仅内存**
  （`execute-server.ts` 的 `executions` Map，key = `idempotency_key`）。Kernel 侧的
  `idempotency_key` 由 Runtime 生成（如 `feishu-reply:<run_id>`，`runtime/mod.rs:247`），
  持久化在 outbox（`outbox_queue.rs:19-42`）。

**后果**：Kernel 在 crash 后重放同一个 outbox row（重新驱动一个 invocation）时，它的
lease/attempts 机制保护 Kernel 侧；但如果 connector 在 dispatch 窗口中重启，connector
的 execute 幂等缓存丢失——一条"已经打到 Feishu、但 Kernel 从未持久化 receipt"的
invocation 被 Kernel 重新驱动时，connector 会**再发一次**，产生重复回复。

这是今天唯一的真实重复回复窗口。Rust 的 unknown policy（terminal-unknown 不自动重发，
PR #66/#69）把这个窗口限制在"Kernel 重放 + connector 恰好重启 + 上一条已到 Feishu 但
receipt 未持久化"这个窄条件里，所以**当前可接受**——但一旦把 connector 拆成独立进程/
服务，这个窗口会被放大（connector 重启频率上升、网络分区更常见）。

## 需要回答的问题

1. 在 Phase 3 extraction 之前，connector-local execute 幂等是否需要持久化？
2. 如果要，最小形态是什么？（JSONL？SQLite？只存 idempotency_key 还是存完整 receipt？）
3. 这个持久化属于 connector（TS）还是 Kernel（Rust）？按 Kernel 边界原则应属于谁？

## 候选方案

### 方案 A：不持久化，明确接受现状（Phase 3 之前）

维持 execute 幂等为内存 Map。理由：

- Rust 的 unknown policy + outbox lease/attempts 已经把重复回复窗口压到很窄；
- Phase 0/1 的 connector 与 Kernel 同机部署，crash 同步发生（一起重启），窗口极小；
- Product roadmap 明确说"协议可以提前想清楚；实现必须等真实失败和重复模式证明后再
  长出来"——目前没有真实重复回复事故的证据。

代价：一旦拆 connector，窗口放大。所以方案 A = **明确 defer**，并在本文记录"拆之前
必须先做 B 或 C"。

### 方案 B：connector-local execute 幂等持久化（JSONL，对称 reaction store）

复用已有 JSONL 模式：把 `execute-server.ts` 的 `executions` Map 换成一个
`execute-idempotency-store.ts`，append-only 记录已完成的 `idempotency_key`（以及可
选的 receipt 摘要），启动时 load，定期 compact。

- 优点：完全在 connector 侧，不碰 Kernel 边界；与 reaction store 同构，实现成本低；
  connector 重启后仍能去重 Kernel 重放的 execute。
- 缺点：多一份磁盘状态要维护；JSONL 只存 key 的话无法返回原 receipt（只能返回
  "replayed"占位，但 Kernel 侧已持久化 receipt 的话无所谓）；需要决定淘汰策略
  （reaction store 无界增长的问题在这里会重现——除非加 TTL / LRU）。

### 方案 C：把 execute 幂等移到 Kernel（Rust outbox）

承认 execute 幂等本质是"这个 invocation 是否已经产出了 receipt"。Kernel 已经在
outbox + Journal 里持有这个事实（`ReceiptReceived` terminal fact）。让 Kernel 在重放
前检查 outbox 是否已经 terminal，避免对已 terminal 的 row 重新驱动 connector。

- 优点：单一事实源（Kernel outbox/Journal），connector 不需要任何 execute 幂等状态；
  符合"connector 是无状态边缘 adapter"的边界原则。
- 缺点：**这其实已经是现状**——`outbox_queue.rs` 的 terminal 守卫
  （`TERMINAL_TRANSITION_ERROR`）和 dispatcher 的 lease 机制已经防止对 terminal row
  重新驱动。缺口只出现在"execute 已到 Feishu、但 Kernel 还没拿到/持久化 receipt"
  的窗口——这个窗口 Kernel 侧无法关闭（它不知道 connector 是否真的发送成功）。

所以方案 C 的结论是：**Kernel 侧已经做到了它能做的极限**，剩余窗口只能由
connector-local 幂等（方案 B）关闭，或在应用层接受（方案 A）。

## 现状（已实现）

**方案 B 已在 `feat/connector-execute-idempotency` 中实现（PR #pending）。**

具体变更：

- **`connectors/feishu/src/execute-store.ts`**：JSONL 持久化 execute 幂等记录，与
  `reaction-store.ts` 对称。记录仅包含 `idempotencyKey`、`invocationId`、
  `operation`、`status`（`sent`/`failed`）、可选的 `receiptSummary`（只含
  `messageId`）、以及时间戳。永不完整个 Feishu 响应、Authorization、token 或 secret。
- **淘汰策略**：记录有 7 天 TTL（`maxAgeMs`），load 时自动淘汰过期条目。JSONL 文件
  达到 256KB 时自动压缩（`compactIfNeeded`），重写为当前 active set。
- **`connectors/feishu/src/execute-server.ts`**：已接受 `ExecuteStore` 参数。在调用
  `sendReply` 前检查持久化 store；成功后将 `"sent"` 记录持久化；失败时**不**持久化。
- **`connectors/feishu/src/config.ts`**：新增 `executeStatePath` 字段，env override
  `AGENT_CORE_FEISHU_EXECUTE_STATE_PATH`，默认 `~/.agent-core/feishu-executes.jsonl`。
- **启动路径**：`index.ts` 在启动时创建 `createJsonlExecuteStore` 并调用
  `store.load()` 预加载，然后传给 `startExecuteServer`。

这意味着 extraction 前置 checklist 的第 1-3 项已完成。第 4 项（IPC 契约文档化）保留
为 extraction PR 的前置条件。

### 未覆盖的边界

- `sendReply` 失败时的 `inFlight` Map 条目被 `catch` 分支 `delete`（现有逻辑），
  但 `failed` 状态的 store 记录**不**被写入，因此重试时仍会调用 `sendReply`。
  这是有意行为：只有 `"sent"` 记录可防止重发。
- TTL 是写死的 7 天，没有运行时配置。这符合 "最小状态" 原则：execute 幂等只在一个
  `OutboxDispatchUnknown` 恢复周期内有用（Kernel 重放窗口）。
- 没有监管删除已 compact 的旧文件。
- `remove_pending` 并发 guard 和 WS 长连接会话仍保持纯内存（不持久化）。

### Extraction 前置 checklist（Phase 3 gate）

剩余的 extraction 前置条件：

1. ~~**实现方案 B**~~ ✅ 已完成
2. ~~**加淘汰策略**~~ ✅ 已完成（TTL + 文件大小 compaction）
3. ~~**回归测试**~~ ✅ 已完成（HTTP 集成测试覆盖重启后幂等去重）
4. **明确契约**：在 IPC 文档里写清 execute 的幂等性由 connector-local 持久化保证，
   ingress 的幂等性由 Kernel Journal 保证（消除当前的不对称隐式假设）。

## 历史

### 原推荐（写于 Phase 3 设计阶段）

**方案 A（明确 defer）作为 Phase 3 之前的状态，并在 extraction 的前置 checklist 里
强制要求方案 B。**

理由：

1. 当前同机部署下窗口极窄，无真实事故证据，不满足 roadmap 的"等重复模式证明"原则；
2. 方案 B 的实现成本不高，但**应该在 connector 拆成独立进程的那个 PR 里一起做**，
   而不是现在（现在做了，同机部署下也无法被验证有用，反而增加要维护的磁盘状态）；
3. 方案 C 已被 Kernel 侧做到极限，无需额外工作。

### 实施决定（Phase 1 increment）

在后续重放测试和稳定性工作（PR #128/#129）中发现，execute 幂等持久化是
replay-eval 管线中"Candidate 对已发出的回复再次发出"的一个真实风险，且实现在
现有 JSONL 模式上的成本已降到足够低，因此决定提前实施方案 B 而不等到 Phase 3。

### 参考

- `connectors/feishu/src/execute-store.ts:28-42`（StoredExecuteRecord 类型，最小字段）；
- `connectors/feishu/src/execute-store.ts:44-51`（ExecuteStore 接口）；
- `connectors/feishu/src/execute-store.ts:66-84`（`createJsonlExecuteStore` 工厂）；
- `connectors/feishu/src/execute-server.ts:38-46`（持久化 store 短路逻辑）；
- `connectors/feishu/src/execute-server.ts:53-63`（成功后的 store.set）；
- `connectors/feishu/src/config.ts:59-62`（`executeStatePath` + env override）；

## 参考

- `connectors/feishu/src/execute-server.ts:6,17,31`（内存 execute 幂等，无淘汰）；
- `connectors/feishu/src/reaction-store.ts:12-18,81-121`（JSONL 持久化 + 压缩）；
- `connectors/feishu/src/reactions.ts:27,166-191`（reaction load + bounded withRetry）；
- `connectors/feishu/src/kernel.ts:4-42,52-53`（ingress，external_event_id 去重）；
- `connectors/feishu/src/config.ts:25-26,66-75`（路径 / 超时配置）；
- `src/adapters/mod.rs:30-55`（Kernel → connector execute，带 idempotency_key）；
- `src/journal/outbox_queue.rs:9,19-42`（outbox terminal 守卫 + idempotency_key 列）；
- `src/runtime/mod.rs:247`（idempotency_key 生成）；
- `docs/product-roadmap.md` Phase 3（connector extraction 目标）。
