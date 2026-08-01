# File-backed Agent Binding V0 — TODO

> 第一步永远是实现 `validation_layout.py`（已完成）。每个阶段完成后运行 `validation_layout.py` 验证，最后一步发送通知。

## 阶段 0：规划产物
- [x] `plan.md` 方案
- [x] `validation_layout.py` 验证脚本
- [x] `todo.md` 本清单

## 阶段 1：bindings 文件与 schema
- [ ] `bindings/feishu.json` 示例文件（version=1，含示例绑定）
- [ ] `bindings/feishu.schema.json` JSON Schema 文档
- [ ] 运行 `validation_layout.py` 验证

## 阶段 2：绑定解析模块
- [ ] `src/binding.rs`：`FeishuBindings` 结构 + `load`（缺失→None）+ `validate`（fail-closed）+ `resolve_agent_id`
- [ ] `src/binding.rs` 单元测试（合法/非法 JSON/version 错误/空字段/重复 chat_id/缺文件/resolve）
- [ ] `src/lib.rs` 注册 `pub mod binding;`
- [ ] 运行 `validation_layout.py` 验证

## 阶段 3：Gateway 路由
- [ ] `src/gateway/mod.rs`：`feishu_agent_id(chat_type, chat_id)` 解析（p2p 不变；群→bindings→默认 agent）
- [ ] `src/gateway/mod.rs`：ingress journal payload 写入 `agent_id`；`recover_feishu_event` 读回（回退默认）
- [ ] `src/gateway/tests.rs`：chat_id→agent_id→SessionTarget.agent_id / 未知 chat_id / p2p 不变 / 非法文件 fail-closed / recover 一致
- [ ] 运行 `validation_layout.py` 验证

## 阶段 4：per-agent Context
- [ ] `src/domain/context_block.rs`：新增 `ContextBlockKind::WorkspaceRoot`
- [ ] `src/context.rs`：AgentProfile 按 `session.agent_id` 解析 + WorkspaceRoot block
- [ ] per-agent 隔离测试（main 与 worker-a 读到不同 AGENT.md / workspace）
- [ ] 运行 `validation_layout.py` 验证

## 阶段 5：全量验证
- [ ] `cargo test -p agent-core-kernel --lib` 全量通过
- [ ] 最终运行 `validation_layout.py`

## 阶段 6：收尾
- [ ] 发送飞书完成通知
