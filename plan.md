# Plan: File-backed Agent Binding V0（飞书群 → Agent 路由）

## 1. 背景

目标：让不同飞书群（chat_id）路由到不同 agent。
`飞书群 chat_id → bindings/feishu.json → agent_id → ~/.agent-core/agents/<agent_id> → 独立上下文、Session、记忆`。

Kernel 保持薄：不改 Run/Session/Journal 语义，不加通用 Router / Memory 系统。只做两件事：
1. ingress 阶段按 `chat_id → agent_id` 解析，填进 `SessionTarget.agent_id`（Session 天然按 agent_id 隔离）。
2. Context 组装按 `session.agent_id` 读取各自 `agents/<agent_id>/` 目录（AGENT.md + workspace），实现 per-agent 上下文隔离。

## 2. 行为决策（fail-closed 边界）

| 场景 | 行为 |
|---|---|
| bindings 文件不存在 | 视为「未配置路由」，全部走默认 agent（`config.agent_id`），向后兼容 |
| bindings 文件存在但非法（JSON 解析失败 / version≠1 / 空字段 / 重复 chat_id） | **fail-closed**：明确报错，拒绝该群消息（`invalid_feishu_bindings`） |
| 群 chat_id 命中绑定 | agent_id = 绑定值 |
| 群 chat_id 未命中 | 默认 agent（`config.agent_id`），明确并有测试 |
| p2p（open_id）路径 | **行为不变**，始终走 `config.agent_id`，有测试 |

## 3. 绑定文件位置

运行时读取 `config.data_dir/bindings/feishu.json`（生产即 `~/.agent-core/bindings/feishu.json`）。
仓库内 `bindings/feishu.json` 为 schema/示例文件（含 `feishu.schema.json` 作为 JSON Schema 文档）。

## 4. 数据结构

```json
{
  "version": 1,
  "bindings": [
    { "chat_id": "oc_xxx", "agent_id": "worker-a" }
  ]
}
```

## 5. 改动点与文件清单

### 新增
- `bindings/feishu.json` — schema 示例（含一个示例绑定 + 注释性说明在 README）。
- `bindings/feishu.schema.json` — JSON Schema（draft-07）文档。
- `src/binding.rs` — `FeishuBindings`：`load(path) -> Result<Option<Self>>`（缺失→None）、`validate()`（fail-closed）、`resolve_agent_id(chat_id) -> Option<AgentId>`；含单元测试。
- `src/gateway/tests.rs` — gateway 路由测试（chat_id→agent_id→SessionTarget.agent_id、未知 chat_id、p2p 不变、非法文件 fail-closed、恢复路径一致）。

### 修改
- `src/lib.rs` — 注册 `pub mod binding;`。
- `src/domain/context_block.rs` — 新增 `ContextBlockKind::WorkspaceRoot`（非破坏，无 exhaustive match）。
- `src/gateway/mod.rs` — 群消息 ingress 前调用 `feishu_agent_id(chat_type, chat_id)`；把解析出的 `agent_id` 写入 ingress journal payload；`recover_feishu_event` 读回该字段（旧事件无此字段 → 回退 `config.agent_id`，行为不变）。
- `src/context.rs` — AgentProfile 由硬编码 `agents/main/AGENT.md` 改为 `agents/{session.agent_id}/AGENT.md`；新增 WorkspaceRoot block 指向 `agents/{agent_id}/workspace/`。

### 不修改
- `src/journal/*`（Session 派生逻辑不动）。
- `src/runtime/*` 既有语义。
- 既有测试契约。

## 6. 验证

1. `validation_layout.py` 检查交付物文件存在、`bindings/` 无多余文件、示例 JSON 合法。
2. `cargo test -p agent-core-kernel --lib` 全量通过。
3. 关键单测：
   - binding 解析/校验（合法、非法、重复、缺字段、缺文件）
   - gateway：群 chat_id→agent_id→SessionTarget.agent_id
   - gateway：未知 chat_id → 默认 agent
   - gateway：p2p 不变
   - gateway：非法 bindings 文件 fail-closed
   - gateway：recover 后 SessionTarget.agent_id 与原始一致
   - context：不同 agent 读到不同 AGENT.md / workspace（隔离）
