#!/usr/bin/env bash
# 一键起 CEF dev 模式（架构 §8.5 M2，feature cef-preview）。
#
# 解决 dev 模式跑裸二进制（target/debug/hebbian，旁无 .app/Frameworks）时 CEF 找不到
# framework / helper 的问题：编 helper bin → 组成 helper.app（macOS 要求 CEF 子进程是
# .app bundle）→ 用 HEBBIAN_CEF_DIR / HEBBIAN_CEF_HELPER 显式指路 → 起 tauri dev。
#
# 用法: bash apps/desktop/cef-dev.sh
# 前置: CEF_PATH 指向 cef-dir（含 Chromium Embedded Framework.framework）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CEF_DIR="${CEF_PATH:-$HOME/code/ricardo/rust/cef-poc/cef-dir}"

if [ ! -d "$CEF_DIR/Chromium Embedded Framework.framework" ]; then
  echo "✗ 找不到 CEF framework: $CEF_DIR" >&2
  echo "  先 export: cd cef-rs && cargo run -p export-cef-dir -- <目标目录>" >&2
  exit 1
fi
echo "[cef-dev] CEF_DIR=$CEF_DIR"

# 1. 编 helper bin
echo "[cef-dev] 编 helper bin..."
cd "$REPO_ROOT"
CEF_PATH="$CEF_DIR" cargo build -p hebbian --bin hebbian-cef-helper --features cef-preview
HELPER_BIN="$REPO_ROOT/target/debug/hebbian-cef-helper"

# 2. 组 helper.app（dev 临时放 target/debug，主进程经 HEBBIAN_CEF_HELPER 指向它）
HELPER_APP="$REPO_ROOT/target/debug/Hebbian Helper.app"
echo "[cef-dev] 组 helper.app -> $HELPER_APP"
rm -rf "$HELPER_APP"
mkdir -p "$HELPER_APP/Contents/MacOS"
cp "$HELPER_BIN" "$HELPER_APP/Contents/MacOS/Hebbian Helper"
cat > "$HELPER_APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>Hebbian Helper</string>
  <key>CFBundleIdentifier</key><string>com.hebbian.cef.helper</string>
  <key>CFBundleName</key><string>Hebbian Helper</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSUIElement</key><string>1</string>
</dict>
</plist>
PLIST
codesign --force --sign - "$HELPER_APP" 2>/dev/null || true

# 3. 起 tauri dev（feature cef-preview + 环境变量指路）
echo "[cef-dev] 起 tauri dev --features cef-preview ..."
cd "$REPO_ROOT/apps/desktop"
export HEBBIAN_CEF_DIR="$CEF_DIR"
export HEBBIAN_CEF_HELPER="$HELPER_APP/Contents/MacOS/Hebbian Helper"
exec pnpm tauri dev --features cef-preview
