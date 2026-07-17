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

# 精准禁用标识列表 — Kernel 源码中不应出现这些字面
# （注释和文档字符串中的引用是允许的，但最好避免）
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

has_violation=false

for pattern in "${BANNED_PATTERNS[@]}"; do
    matching_files=$(grep -rl "$pattern" "$KERNEL_SRC" --include="*.rs" 2>/dev/null || true)
    if [ -n "$matching_files" ]; then
        for file in $matching_files; do
            # 检查是否在允许列表中
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

if [ "$has_violation" = true ]; then
    echo ""
    echo "Kernel 负面宪法检查失败。"
    echo "以上标识符不应出现在 Kernel 代码中。"
    echo "请将相关功能移至外部 Harness。"
    exit 1
fi

echo "✅ Kernel 负面宪法检查通过。"
exit 0
