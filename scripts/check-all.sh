#!/usr/bin/env bash
# 一条命令把两个 workspace 都过一遍。
#
# 为什么要这个脚本：apps/gpui 自成一个 workspace（原因见仓库根 Cargo.toml 顶部注释），
# 所以根目录的 `cargo check --workspace` **覆盖不到它**。少跑那一条很容易漏掉编译错误。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> 主 workspace"
cargo check --workspace "$@"

echo "==> apps/gpui（独立 workspace）"
cd apps/gpui && cargo check "$@" && cargo test "$@"
