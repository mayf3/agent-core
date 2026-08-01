# Coding Harness 持久 Job 与分段自动续跑 V0

状态：实现完成，见 `tools/coding-harness/src/jobs.rs`、`src/opencode_backend.rs`、
`src/server.rs`。全部位于外部 Development/Coding Harness，**Agent Core Kernel 零改动**。

## 北极星

```text
飞书提交开发任务
→ Development Harness 快速受理并返回 job_id
→ 当前 Kernel Run 结束
→ Harness 在外部自主调查、实现、测试和提交
→ 超过单段预算时自动保存 checkpoint 并继续下一段
→ 完成、失败或需要审批时通知
```

不再要求用户为了工具轮数或 300 秒限制手工发送“继续”。

## 现状与复用

改造前 `external.coding_task_submit` 已支持 submit / task_id / status query /
cancel，但任务只存在进程内 `HashMap`，opencode 后端是单次最长 600s 的
`opencode run`——没有持久化、checkpoint、分段、恢复与自动续跑。

V0 复用了：

- `external-harness-v1` 协议与 `external.coding_task_submit` /
  `external.coding_task_status` 操作名（响应新增 `job_id`/`accepted_at`/
  `task_digest`，`task_id` 保留为别名）；
- workspace 权限模型（`CODING_CONFIG`）与 opencode 进程组清理机制。

## 持久 Job

提交后立即返回：

```json
{
  "job_id": "job_…",
  "task_id": "job_…",
  "status": "accepted",
  "accepted_at": "…",
  "task_digest": "sha256…",
  "backend": "fake|opencode",
  "workspace_id": "…",
  "segment_budget": { … }
}
```

Job 持久化为 `<HARNESS_JOB_STORE>/jobs/<job_id>.json`（默认
`<HARNESS_ARTIFACT_ROOT>/jobs`，原子写：临时文件 + rename）。字段：

```text
job_id, task(objective/acceptance/backend/model), status, current_phase,
checkpoint, attempt, created_at, accepted_at, updated_at, last_error,
result_summary, task_digest, segments[], finalize
```

状态集合：`accepted / running / waiting_approval / completed / failed / cancelled`。
Harness 进程重启后，`start_scheduler` 先 `recover_all`：`running` → `accepted`
并追加 `interrupted` 段回执，然后自动继续。

## 分段执行

每个执行段独立受限（`ExecutionSegmentBudget`）：

```text
max_model_rounds       模型轮数上限（opencode 按 JSON 事件流实时计数）
max_wall_time_ms       段墙钟上限（宿主管控，超时杀进程组）
max_tool_calls         工具调用上限
single_tool_timeout_ms 段内无新事件静默上限（卡死熔断）
on_exhaustion          checkpoint_and_continue | stop_failed | request_approval
```

预算解析优先级：workspace `segment_budget`（CODING_CONFIG）→
`HARNESS_SEGMENT_BUDGET` 环境变量 → 内置默认
（100 轮 / 300s / 200 工具 / 120s 静默 / checkpoint_and_continue）。
任何解析结果都会被宿主安全熔断
（`HARNESS_HOST_SAFETY_CEILING`，内置 1000 轮 / 3600s / 2000 工具 / 600s）
逐字段封顶；另有每 Job 50 段的总熔断。

段耗尽时：保存 checkpoint → 记录 segment receipt（outcome=exhausted +
冻结的预算决策 digest）→ 由调度器自动开始下一段。**不需要用户发送“继续”**，
“段结束”不等于“任务完成”。

### 预算 Hook

- 内置 Hook：`builtin:segment-budget-default-v0`（v0）。
- 每段开始时冻结 hook 身份、版本与决策 digest
  （sha256(hook_id + hook_version + attempt + resolved budget)），写入段回执。
- 模型不能自行覆盖预算：提交参数里的 `segment_budget` 与解析结果不一致时
  返回 `budget_override_rejected`。
- Hook 不能突破宿主熔断：配置级封顶 + 段数熔断 + 墙钟硬杀。
- 不通过临时修改环境变量或无限调大 timeout 冒充续跑。

## Checkpoint 最小范围

```text
objective / boundaries            当前目标与边界（段间不变）
findings                          已调查结论（累计）
workspace {repository,branch,HEAD,working_tree_digest}  工作树事实
completed_steps / remaining_steps 已完成 / 剩余步骤
last_test_result                  最近测试结果
blocker / next_action             当前 Blocker / 下一步动作
```

代码、Git 提交与测试产物仍以真实 workspace/repository 为权威，Job 数据库不
复制完整代码。恢复（第 2 段起）必须先核对 repository/branch/HEAD/工作树
digest；漂移则停止并报告（`failed` + `checkpoint_drift`），不盲目继续。

opencode 后端在每段 prompt 末尾要求模型输出固定 key 的 JSON checkpoint
块，Harness 从输出中提取最后一块作为续跑上下文；fake 后端按脚本确定性
生成 checkpoint（用于测试与验收）。

## 与 Kernel 边界

Kernel 只负责：身份与权限、提交 Invocation、Accepted Receipt、
Approval/Decision、Completion/Failure Receipt、最终飞书通知。
Development Harness 负责：Job 状态、调查与计划、开发和测试、checkpoint、
分段自动续跑、进程重启恢复、Git 提交和 PR。

Kernel 侧 `development_request`（组件生成）同步路径未改动；本 V0 只升级
workspace 路径的 `external.coding_task_submit`。PR #219 的 Run Budget Hook
只治理 Kernel 同步 Run，与本任务无依赖。

## API

| 操作 | 说明 |
|---|---|
| `external.coding_task_submit` | 提交持久 Job，快速返回 accepted 回执 |
| `external.coding_task_status` | 完整 Job 视图（checkpoint/segments/冻结预算） |
| `external.coding_task_cancel` | 取消（含中断在飞段） |
| `external.coding_task_resume` | waiting_approval → accepted |

提交参数（`arguments`）：

```json
{
  "workspace_id": "…",
  "objective": "…",
  "acceptance_criteria": "…",
  "backend": "opencode|fake",
  "model": "deepseek/deepseek-v4-flash",
  "finalize": {
    "create_pr": true,
    "pr_title": "…",
    "pr_body": "…",
    "base_branch": "main",
    "branch": "codex/job-…"
  }
}
```

`finalize.create_pr` 时：第 1 段前创建 job 分支（git 非仓库 → 提交即失败
`pr_requires_git_workspace`）；任务完成后 push 分支并 `gh pr create`
（永不自动合并）。

## 通知

终态（completed/failed/cancelled）与 waiting_approval 写通知记录到
`<HARNESS_JOB_STORE>/notifications/`；配置 `HARNESS_FEISHU_WEBHOOK_URL`
时额外 POST 到飞书 webhook（5s 超时，失败不阻断 Job）。

## 验收（真实验收脚本）

见 `tools/coding-harness/tests/jobs_segmented_e2e.rs`（自动续跑、冻结预算、
override 拒绝、重启恢复、漂移、cancel/resume），以及下方真实二进制验收：

```text
第一段预算：max_model_rounds=2, max_wall_time_ms=60000,
           on_exhaustion=checkpoint_and_continue
验收点：
1. 提交后快速返回真实 job_id（status=accepted + accepted_at + task_digest）
2. 当前 Kernel Run 可以结束（提交不阻塞）
3. 第一段耗尽后写入 checkpoint（completed/remaining steps 持久化）
4. 无需用户发送“继续”
5. Harness 自动开始第二段（attempt 递增、段回执 outcome=exhausted）
6. Harness 重启后仍能继续（recover_all + interrupted 回执）
7. 最终产生 Completion Receipt（completed + result_summary + 通知记录）
8. 不把“第一段结束”描述成“任务完成”（状态仍是 accepted 而非 completed）
```
