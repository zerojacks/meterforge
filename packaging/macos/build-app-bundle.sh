#!/usr/bin/env sh
# 把已经编译好的 MeterForge 二进制打包成标准的 macOS .app：
# cargo 本身只产出一个裸的 Mach-O 可执行文件，Dock/访达要显示图标，
# 必须有 Contents/Info.plist + Contents/Resources/app.icns 这层 .app 包装。
#
# 用法:
#   cargo build -p meter-ui --release
#   sh packaging/macos/build-app-bundle.sh          # 默认 release
#   sh packaging/macos/build-app-bundle.sh debug    # 或指定 debug
#
# 可选环境变量（CI 打 DMG 时使用）:
#   TARGET_TRIPLE  交叉编译三元组（如 x86_64-apple-darwin / aarch64-apple-darwin）。
#                  设置后从 target/<triple>/<profile>/ 取二进制，
#                  对应 cargo build --target <triple> 的输出位置。
#   APP_VERSION    写入 Info.plist 的版本号（CFBundleVersion /
#                  CFBundleShortVersionString），不设置则保留模板里的 0.1.0。
set -eu

APP_NAME="MeterForge"
BIN_NAME="MeterForge"
PROFILE_ARG="${1:-release}"
TARGET_TRIPLE="${TARGET_TRIPLE:-}"
APP_VERSION="${APP_VERSION:-}"
# cargo 的 --profile release 输出目录是 target/release，其它自定义 profile
# 目录名与 profile 名一致，这里只处理最常见的 debug/release 两种。
case "$PROFILE_ARG" in
  release) PROFILE_DIR="release" ;;
  debug)   PROFILE_DIR="debug" ;;
  *)       PROFILE_DIR="$PROFILE_ARG" ;;
esac

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
# 交叉编译时二进制在 target/<triple>/<profile>/ 下
TARGET_BIN="$ROOT_DIR/target/${TARGET_TRIPLE:+$TARGET_TRIPLE/}$PROFILE_DIR/$BIN_NAME"

if [ ! -f "$TARGET_BIN" ]; then
  echo "未找到 $TARGET_BIN" >&2
  if [ -n "$TARGET_TRIPLE" ]; then
    echo "请先执行: cargo build -p meter-ui --profile $PROFILE_ARG --target $TARGET_TRIPLE" >&2
  else
    echo "请先执行: cargo build -p meter-ui --profile $PROFILE_ARG" >&2
  fi
  exit 1
fi

# .app 固定产在 target/<profile>/ 下（不随 triple 变化，调用方路径简单）
APP_BUNDLE="$ROOT_DIR/target/$PROFILE_DIR/$APP_NAME.app"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"

cp "$TARGET_BIN" "$APP_BUNDLE/Contents/MacOS/$BIN_NAME"
cp "$ROOT_DIR/meterforge-ui/assets/icon/app.icns" "$APP_BUNDLE/Contents/Resources/app.icns"
if [ -n "$APP_VERSION" ]; then
  sed -e "s/__BIN_NAME__/$BIN_NAME/" -e "s/0\.1\.0/$APP_VERSION/g" \
    "$SCRIPT_DIR/Info.plist.in" > "$APP_BUNDLE/Contents/Info.plist"
else
  sed "s/__BIN_NAME__/$BIN_NAME/" "$SCRIPT_DIR/Info.plist.in" > "$APP_BUNDLE/Contents/Info.plist"
fi

echo "已生成 $APP_BUNDLE"
echo "可以直接双击运行，或拖进 /Applications。"
