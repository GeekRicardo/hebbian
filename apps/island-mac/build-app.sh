#!/usr/bin/env bash
# 把 hebisland Swift Package 组装成 HebIsland.app。
#
# 为什么要 .app 而非裸二进制：CardView 用 Bundle.module 加载彩色图标 PNG，
# SPM 生成的 Bundle.module 第一优先路径是「可执行文件所在 .app 根 / HebIsland_HebIsland.bundle」，
# 找不到才回退到编译机硬编码路径（在用户机上不存在 → fatalError 崩溃）。
# 因此资源 bundle 必须随二进制一起装进 .app，裸 sidecar 单文件做不到。
#
# 产物：apps/island-mac/dist/HebIsland.app，由 Tauri bundle.resources 整体嵌进 Hebbian.app。
set -euo pipefail

cd "$(dirname "$0")"

CONFIG="${1:-release}"
BUILD_DIR=".build/${CONFIG}"
APP="dist/HebIsland.app"

echo "[hebisland] swift build -c ${CONFIG}"
swift build -c "${CONFIG}"

BIN="${BUILD_DIR}/hebisland"
BUNDLE="${BUILD_DIR}/HebIsland_HebIsland.bundle"
[ -x "${BIN}" ] || { echo "缺少二进制 ${BIN}"; exit 1; }
[ -d "${BUNDLE}" ] || { echo "缺少资源 bundle ${BUNDLE}"; exit 1; }

echo "[hebisland] 组装 ${APP}"
rm -rf "${APP}"
mkdir -p "${APP}/Contents/MacOS"
cp "${BIN}" "${APP}/Contents/MacOS/hebisland"
cp Info.plist "${APP}/Contents/Info.plist"
# bundle 放 .app 根（Bundle.main.bundleURL 级），与 Bundle.module 的第一优先路径对齐。
cp -R "${BUNDLE}" "${APP}/HebIsland_HebIsland.bundle"
chmod +x "${APP}/Contents/MacOS/hebisland"

echo "[hebisland] done -> ${APP}"
