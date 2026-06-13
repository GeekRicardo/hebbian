#!/usr/bin/env bash
# CEF 打包后处理（架构 §8.5 M2，feature cef-preview）。
#
# tauri build 产出的 Hebbian.app 默认只有主二进制 + wry。本脚本把 CEF runtime 组进去：
#   1. 复制 Chromium Embedded Framework.framework → Contents/Frameworks/
#   2. 用已编译的 hebbian-cef-helper 组 5 个 helper.app（GPU/Renderer/Plugin/Alerts/默认）
#   3. 嵌套签名（helper → framework → 外层，顺序不能反）
#
# 由 tauri afterBuild hook 调用（见 tauri.conf.json）。需环境变量 CEF_PATH 指向 cef-dir。
#
# ⚠️ 整套产物正确性必须真机 build + 跑验证：bundle 结构 / rpath / 签名顺序 / helper
# 进程能否被主进程拉起——任一错都黑屏或崩，无法在非 build 环境验证。
set -euo pipefail

APP_PATH="${1:?用法: cef-bundle.sh <Hebbian.app 路径>}"
CEF_DIR="${CEF_PATH:?需设 CEF_PATH 指向 cef-dir（含 Chromium Embedded Framework.framework）}"
HELPER_BIN="${2:?用法: cef-bundle.sh <app> <hebbian-cef-helper 二进制路径>}"

FRAMEWORK="Chromium Embedded Framework.framework"
FRAMEWORKS_DIR="$APP_PATH/Contents/Frameworks"
BUNDLE_ID_BASE="com.hebbian.cef.helper"

echo "[cef-bundle] 复制 framework → $FRAMEWORKS_DIR"
mkdir -p "$FRAMEWORKS_DIR"
rm -rf "$FRAMEWORKS_DIR/$FRAMEWORK"
cp -R "$CEF_DIR/$FRAMEWORK" "$FRAMEWORKS_DIR/$FRAMEWORK"

# CEF 5 个 helper 变体：名字后缀 + LSUIElement（后台进程不进 dock）。
# 主默认 helper 名 "Hebbian Helper"，其余带 (GPU)/(Renderer) 等后缀。
make_helper() {
  local suffix="$1"      # "" / " (GPU)" / " (Renderer)" / " (Plugin)" / " (Alerts)"
  local name="Hebbian Helper${suffix}"
  local app="$FRAMEWORKS_DIR/${name}.app"
  local id_suffix="${suffix//[^A-Za-z]/}"
  echo "[cef-bundle] 组 helper: ${name}.app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS"
  cp "$HELPER_BIN" "$app/Contents/MacOS/${name}"
  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>${name}</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID_BASE}${id_suffix}</string>
  <key>CFBundleName</key><string>${name}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSUIElement</key><string>1</string>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
}

for suffix in "" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)"; do
  make_helper "$suffix"
done

# 嵌套签名（ad-hoc）：内层先签——helper.app → framework → 外层 Hebbian.app。
# 顺序反了外层签名会因内层未签而失效。release 正式签名换成开发者证书。
echo "[cef-bundle] ad-hoc 签名（内→外）"
for app in "$FRAMEWORKS_DIR"/*Helper*.app; do
  codesign --force --sign - --timestamp=none "$app"
done
codesign --force --sign - --timestamp=none "$FRAMEWORKS_DIR/$FRAMEWORK"
codesign --force --sign - --timestamp=none "$APP_PATH"

echo "[cef-bundle] 完成。CEF 已组进 $APP_PATH"
