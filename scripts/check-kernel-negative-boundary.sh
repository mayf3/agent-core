#!/usr/bin/env bash
# Kernel 负面宪法边界检查脚本
#
# 检查 src/ 下是否出现禁用标识符。这些标识符表示 Kernel
# 可能开始理解不应该知道的业务域概念。
#
# 使用：
#   bash scripts/check-kernel-negative-boundary.sh

set -euo pipefail

KERNEL_SRC="src"
has_violation=false

# ── 1. 全局禁用标识 ──────────────────────────────────
# Kernel 全局不得出现这些标识符（已移除的版本分配函数）
GLOBAL_BANNED=(
    "resolve_next_deployment_version"
    "increment_patch"
)
for pattern in "${GLOBAL_BANNED[@]}"; do
    matching_files=$(grep -rl "$pattern" "$KERNEL_SRC" --include="*.rs" 2>/dev/null || true)
    if [ -n "$matching_files" ]; then
        for file in $matching_files; do
            echo "❌ 全局禁用标识符 '$pattern' 在: $file"
            has_violation=true
        done
    fi
done

# ── 2. 业务域禁用标识 ────────────────────────────────
# Kernel 源码中不应出现这些业务域概念字面
BANNED_PATTERNS=(
    "token-dashboard-v0"
    "failure-event-viewer-v0"
    "AcceptanceKit("
    "PublicSpec("
    "PrivateVerifier("
)
# 允许的例外（按文件路径）
ALLOWED_EXCEPTIONS=(
    "src/server/coding_router.rs"   # 该文件即将删除相关引用
)

for pattern in "${BANNED_PATTERNS[@]}"; do
    matching_files=$(grep -rl "$pattern" "$KERNEL_SRC" --include="*.rs" 2>/dev/null || true)
    if [ -n "$matching_files" ]; then
        for file in $matching_files; do
            is_allowed=false
            for allowed in "${ALLOWED_EXCEPTIONS[@]}"; do
                if [ "$file" = "$allowed" ]; then
                    is_allowed=true
                    break
                fi
            done
            if [ "$is_allowed" = false ]; then
                echo "❌ 发现禁用标识符 '$pattern' 在: $file"
                has_violation=true
            fi
        done
    fi
done

# ── 3. coding_task_submit 模块专项检查 ────────────────
# 开发提交路径不得再理解服务版本、Manifest业务格式或
# 查询 Deployment Harness 开发版本端点。
CODING_SUBMIT_DIR="$KERNEL_SRC/server/coding_task_submit"
if [ -d "$CODING_SUBMIT_DIR" ]; then
    CODING_BANNED=(
        "resolve_next_deployment_version"
        "increment_patch"
        "GET /v1/components/"
        "AGENT_CORE_DEPLOYMENT_HARNESS_CONTROL_URL"
        "ServiceManifest"
        "service.version"
        "service.*version"
    )
    for pattern in "${CODING_BANNED[@]}"; do
        matching_files=$(grep -rl "$pattern" "$CODING_SUBMIT_DIR" --include="*.rs" 2>/dev/null || true)
        if [ -n "$matching_files" ]; then
            for file in $matching_files; do
                echo "❌ coding_task_submit 禁用标识符 '$pattern' 在: $file"
                has_violation=true
            done
        fi
    done
fi

# ── 4. V2 InvocableCapability 构造专项检查 ──────────
# Kernel 开发提交路径不得再构造 HarnessManifest 或理解
# InvocableCapability 产品字段（已移至 Coding Harness）。
V2_BANNED=(
    "invocable_manifest("
    "HarnessManifest {"
    "capability-host-v0"
    "127.0.0.1:7300/execute"
    "CAPABILITY_INPUT_SCHEMA_MISSING"
    "CAPABILITY_OUTPUT_SCHEMA_MISSING"
    "CAPABILITY_IDEMPOTENCY_MISSING"
)
# 允许的例外（运行期合法的 HarnessManifest 读取与调用）
V2_ALLOWED_EXCEPTIONS=(
    # capability_routes.rs 在激活时需要读取已批准 HarnessManifest
    "src/server/capability_routes.rs"
    # harness_routes.rs 注册/查询 HarnessManifest
    "src/server/harness_routes.rs"
    # capability_routes_support.rs 支持能力路由
    "src/server/capability_routes_support.rs"
    # HarnessManifest 类型定义
    "src/harness/manifest.rs"
    # 运行期激活
    "src/journal/activation_core.rs"
    "src/journal/harness_ops.rs"
    "src/journal/capability_activation.rs"
    "src/journal/trusted_capability_activation.rs"
    # 运行期决策
    "src/server/capability_decision.rs"
    # domain 类型定义包含 deployment_profile 值
    "src/domain/self_evolution.rs"
)
# 同时允许以下情况：
# - 测试文件中的 HarnessManifest 构造（运行期测试需要使用 fixture）
# - 仅含注释的匹配（不以实际 Rust 标识符形式出现）
for pattern in "${V2_BANNED[@]}"; do
    matching_files=$(grep -rl "$pattern" "$KERNEL_SRC" --include="*.rs" 2>/dev/null || true)
    if [ -n "$matching_files" ]; then
        for file in $matching_files; do
            is_allowed=false
            for allowed in "${V2_ALLOWED_EXCEPTIONS[@]}"; do
                if [ "$file" = "$allowed" ]; then
                    is_allowed=true
                    break
                fi
            done
            # Also allow test files (runtime tests may construct HarnessManifest for fixtures)
            if [[ "$file" == *"/tests/"* ]] || [[ "$file" == *"tests.rs" ]]; then
                is_allowed=true
            fi
            # If still flagged, check whether ALL matches are in comments only
            if [ "$is_allowed" = false ]; then
                # Get non-comment matches by filtering out lines with //
                non_comment=$(grep -n "$pattern" "$file" 2>/dev/null | grep -v "//" || true)
                if [ -z "$non_comment" ]; then
                    is_allowed=true
                fi
            fi
            if [ "$is_allowed" = false ]; then
                echo "❌ V2 禁止标识符 '$pattern' 在: $file"
                has_violation=true
            fi
        done
    fi
done

if [ "$has_violation" = true ]; then
    echo ""
    echo "Kernel 负面宪法检查失败。"
    echo "以上标识符不应出现在 Kernel 代码中。"
    echo "请将相关功能移至外部 Harness。"
    exit 1
fi

echo "✅ Kernel 负面宪法检查通过。"
exit 0
