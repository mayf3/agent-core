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

if [ "$has_violation" = true ]; then
    echo ""
    echo "Kernel 负面宪法检查失败。"
    echo "以上标识符不应出现在 Kernel 代码中。"
    echo "请将相关功能移至外部 Harness。"
    exit 1
fi

echo "✅ Kernel 负面宪法检查通过。"
exit 0
