#!/usr/bin/env bash
# 构造 SWE-bench 风格 sample 用的本地最小 git repo（不提交进主仓库，运行前生成）。
# 用法：bash make-swe-repo.sh [目标目录]，默认 /tmp/heb-eval-swe-repo
set -euo pipefail

DEST="${1:-/tmp/heb-eval-swe-repo}"
rm -rf "$DEST"
mkdir -p "$DEST"
cd "$DEST"

git init -q
git config user.email "eval@hebbian.local"
git config user.name "heb-eval"

# 有 bug 的实现：multiply 实际做了加法。
printf 'def multiply(a, b):\n    return a + b\n' > calc.py
git add calc.py
git commit -q -m "init: calc with buggy multiply"

echo "SWE sample repo 已生成：$DEST（HEAD=$(git rev-parse --short HEAD)）"
