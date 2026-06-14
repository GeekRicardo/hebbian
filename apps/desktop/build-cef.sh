#!/usr/bin/env bash
# 一键 build CEF 版 Hebbian.app + DMG（架构 §8.5 M2，feature cef-preview）。
#
# 不动原 `pnpm tauri build`（仍出 wry 版）。本脚本流程：
#   1. tauri build --features cef-preview --bundles app  → 出基础 .app（仅 .app，不打 DMG）
#   2. cef-bundle.sh  → 把 CEF framework + helper.app ×5 组进 .app，嵌套签名
#   3. hdiutil  → 把组好 CEF 的 .app 打成 DMG
#
# 用法: bash apps/desktop/build-cef.sh
# 前置: CEF_PATH 指向 cef-dir（含 Chromium Embedded Framework.framework）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CEF_DIR="${CEF_PATH:-$HOME/code/ricardo/rust/cef-poc/cef-dir}"
export CEF_PATH="$CEF_DIR"

if [ ! -d "$CEF_DIR/Chromium Embedded Framework.framework" ]; then
  echo "✗ 找不到 CEF framework: $CEF_DIR" >&2
  echo "  先 export: cd cef-rs && cargo run -p export-cef-dir -- <目标目录>" >&2
  exit 1
fi
echo "[build-cef] CEF_DIR=$CEF_DIR"

BUNDLE_DIR="$REPO_ROOT/target/release/bundle/macos"
APP="$BUNDLE_DIR/Hebbian.app"
HELPER_BIN="$REPO_ROOT/target/release/hebbian-cef-helper"

# 1. tauri build（feature cef-preview，只出 .app；DMG 推迟到组完 CEF 再打）
# --config 叠加 mainBinaryName=hebbian：项目有两个 [[bin]]（主 + helper），不指明
# tauri 无法确定哪个是 app 主二进制。
echo "[build-cef] tauri build --features cef-preview（只出 .app）..."
cd "$REPO_ROOT/apps/desktop"
pnpm tauri build --features cef-preview --bundles app --config tauri.cef.conf.json

# helper bin 是 release 版（tauri build 已编 release，但 helper 是独立 bin 要单独确认）
if [ ! -f "$HELPER_BIN" ]; then
  echo "[build-cef] 单独编 helper bin（release）..."
  cd "$REPO_ROOT"
  cargo build --release -p hebbian --bin hebbian-cef-helper --features cef-preview
fi

# 2. 组 CEF 进 .app
echo "[build-cef] 组 CEF runtime 进 $APP ..."
bash "$REPO_ROOT/apps/desktop/cef-bundle.sh" "$APP" "$HELPER_BIN"

# 3. 打 DMG（简单 hdiutil；release 正式分发可换 create-dmg 美化）
DMG="$BUNDLE_DIR/Hebbian-cef.dmg"
echo "[build-cef] 打 DMG -> $DMG ..."
rm -f "$DMG"
hdiutil create -volname "Hebbian" -srcfolder "$APP" -ov -format UDZO "$DMG"

echo ""
echo "✓ CEF 版构建完成："
echo "  .app: $APP"
echo "  DMG : $DMG"
echo ""
echo "首次打开需右键→打开绕过 Gatekeeper（ad-hoc 签名未公证）。"
