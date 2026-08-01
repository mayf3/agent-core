#!/usr/bin/env bash
#
# submit-route-harness-job.sh — 提交 File-backed Agent Binding V0 为真实
# Development Harness Job（opencode 后端，分段自动续跑，完成时创建 PR）。
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PORT="${ROUTE_PORT:-7202}"
WS_ROOT="${ROUTE_WS:-$HOME/.agent-core/workspace/route-harness-v0}"
ART_ROOT="${ROUTE_ART:-$HOME/.agent-core/workspace/route-harness-artifacts}"
CONTROL_TOKEN="${ROUTE_CONTROL_TOKEN:-route-harness-control-token}"

[ -d "$WS_ROOT/.git" ] || { echo "workspace $WS_ROOT not a git clone"; exit 1; }

BIN="$REPO_DIR/tools/coding-harness/target/debug/coding-harness"
[ -x "$BIN" ] || { echo "build first: cargo build --manifest-path $REPO_DIR/tools/coding-harness/Cargo.toml"; exit 1; }

export CODING_HARNESS_CONTROL_TOKEN="$CONTROL_TOKEN"
export HARNESS_ARTIFACT_ROOT="$ART_ROOT"
export CODING_CONFIG="{\"workspaces\":{\"agent-core\":{\"root\":\"$WS_ROOT\",\"read\":true,\"write\":true,\"exec\":true,\"opencode\":true,\"network\":true,\"shell\":false}}}"

pkill -f "coding-harness --listen 127.0.0.1:$PORT" 2>/dev/null || true
sleep 0.5
"$BIN" --listen "127.0.0.1:$PORT" >"$ART_ROOT/harness.log" 2>&1 &
HARNESS_PID=$!
mkdir -p "$ART_ROOT"
echo "harness pid=$HARNESS_PID on port $PORT"
for _ in $(seq 1 50); do
  if curl -s -m 1 "http://127.0.0.1:$PORT/execute" -X POST \
      -H "Content-Type: application/json" \
      -d '{"protocol_version":"external-harness-v1","operation":"external.coding_workspace_list","arguments":{"workspace_id":"agent-core"}}' \
      >/dev/null 2>&1; then break; fi
  sleep 0.2
done

OBJECTIVE='# File-backed Agent Binding V0（Route Harness 开发任务）

## 任务目标
实现"飞书群 → Agent"文件绑定，让不同飞书群（chat_id）路由到不同 agent：
飞书群 chat_id → bindings/feishu.json → agent_id → ~/.agent-core/workspace/<agent_id> → 独立上下文、Session 和记忆。

## 已调查结论（来自先前调查，直接使用，不要重新进行无限调查）
1. 飞书入口在 src/gateway/mod.rs 的 validate_feishu_ingress（约 line 220-260）：p2p 的 conversation_key="feishu:open_id:{sender_open_id}"，群为 "feishu:chat_id:{chat_id}"；ValidatedEvent.session_target.agent_id 目前硬编码 self.config.agent_id（src/config.rs 默认 AgentId("main")）。
2. Session 由 SessionTarget{agent_id, channel, conversation_key} 经 journal.get_or_create_session（src/journal/sqlite.rs:179）确定，session_id 由这三个字段派生 —— 改变 agent_id 即得到独立 Session。
3. 上下文组装在 src/context.rs ContextAssembler::build：AgentProfile block 硬编码 "agents/main/AGENT.md"；root_dir 来自 config.root_dir（AGENT_CORE_CONTEXT_DIR）。
4. 当前 ~/.agent-core/agents/main 存在（AGENT.md 等）；~/.agent-core/workspace 尚不存在，需要按 agent_id 创建。
5. bindings/feishu.json 不存在，需要新建（schema、校验、示例、测试）。
6. src/domain/mod.rs SessionTarget 定义在 line 273 附近；RuntimeEventPayload 有 chat_id 字段。

## 设计建议（可调整，保持 Kernel 薄）
- bindings/feishu.json：{"version":1,"bindings":[{"chat_id":"oc_xxx","agent_id":"worker-a"}]}；未知 chat_id 行为需明确（默认 agent 或拒绝，二选一并测试）。
- gateway 构造 SessionTarget 前解析 chat_id → agent_id；保持 p2p 路径（open_id）行为不变。
- per-agent workspace：context/记忆落在各自 ~/.agent-core/workspace/<agent_id>/ 或 data_dir 下按 agent_id 分目录。
- 不改动 Run/Session/Journal 的既有语义；不加通用 Workflow/多 Agent Router/Dreaming/Memory 系统/自动拆解。

## 边界（必须遵守）
- 只修改本 workspace 目录内文件；不得访问外部目录；不得读取 .env、tokens、密钥或生产数据。
- 不得 push/merge/deploy；git 提交后由 Harness 统一创建 PR（base=main），不要自动合并。
- 不要删除或破坏现有测试与契约（schema_tests、gateway 测试等）。

## 验收标准
1. bindings/feishu.json schema 明确且有校验逻辑（非法文件 → 明确错误，fail-closed）。
2. 群消息 ingress 时 agent_id 由 binding 解析，有单元测试证明（chat_id → agent_id → SessionTarget.agent_id）。
3. 未知 chat_id 行为有测试覆盖。
4. p2p（open_id）路径现有行为不变，有测试覆盖。
5. per-agent workspace 按 agent_id 隔离，有测试覆盖。
6. 相关测试通过：cargo test -p agent-core-kernel --lib（或聚焦 gateway/context 相关测试先跑，最终跑全量 lib 测试）。
7. 提交清晰的 commit（可多个），最终由 Harness 创建 PR。不自动合并。

## 下一步动作（从这开始）
先读 src/gateway/mod.rs validate_feishu_ingress 与 src/context.rs、src/config.rs、src/domain/mod.rs 的 SessionTarget，设计最小改动方案（列出文件清单与改动点），实现 + 测试，最后全量验证并提交。'

python3 - "$OBJECTIVE" "$PORT" "$CONTROL_TOKEN" << 'PYEOF'
import json, sys, urllib.request

objective, port, token = sys.argv[1], sys.argv[2], sys.argv[3]
body = {
    "protocol_version": "external-harness-v1",
    "operation": "external.coding_task_submit",
    "arguments": {
        "workspace_id": "agent-core",
        "objective": objective,
        "acceptance_criteria": [
            "bindings/feishu.json schema + 校验（fail-closed）",
            "群 chat_id → agent_id 绑定解析（单测）",
            "未知 chat_id 行为明确并有测试",
            "p2p 路径行为不变（测试）",
            "per-agent workspace 隔离（测试）",
            "cargo test -p agent-core-kernel --lib 通过",
            "不自动合并；Harness 创建 PR"
        ],
        "backend": "opencode",
        "model": "deepseek/deepseek-v4-flash",
        "finalize": {
            "create_pr": True,
            "pr_title": "feat: File-backed Agent Binding V0 — 飞书群 chat_id 绑定到 agent_id 与 per-agent workspace",
            "pr_body": "由 Development Harness 分段 Job 自动完成：调查结论 + 实现 + 测试 + 提交。\n\nFile-backed Agent Binding V0：飞书群 chat_id → bindings/feishu.json → agent_id → ~/.agent-core/workspace/<agent_id> → 独立上下文、Session 和记忆。",
            "base_branch": "main"
        }
    }
}
req = urllib.request.Request(
    f"http://127.0.0.1:{port}/execute",
    data=json.dumps(body).encode(),
    headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
    method="POST",
)
with urllib.request.urlopen(req, timeout=10) as resp:
    result = json.loads(resp.read())
print(json.dumps(result, ensure_ascii=False, indent=2))
if not result.get("ok"):
    sys.exit(1)
PYEOF
echo "HARNESS_PID=$HARNESS_PID"
