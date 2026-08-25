#!/usr/bin/env sh
# 把已经编译好的 meter-ui 二进制打包成标准的 macOS .app：
# cargo 本身只产出一个裸的 Mach-O 可执行文件，Dock/访达要显示图标，
# 必须有 Contents/Info.plist + Contents/Resources/app.icns 这层 .app 包装。
#
# 用法:
#   cargo build -p meter-ui --release
#   sh packaging/macos/build-app-bundle.sh          # 默认 release
#   sh packaging/macos/build-app-bundle.sh debug    # 或指定 debug
set -eu

APP_NAME="Meter Engine"
BIN_NAME="meter-ui"
PROFILE_ARG="${1:-release}"
# cargo 的 --profile release 输出目录是 target/release，其它自定义 profile
# 目录名与 profile 名一致，这里只处理最常见的 debug/release 两种。
case "$PROFILE_ARG" in
  release) PROFILE_DIR="release" ;;
  debug)   PROFILE_DIR="debug" ;;
  *)       PROFILE_DIR="$PROFILE_ARG" ;;
esac

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
TARGET_BIN="$ROOT_DIR/target/$PROFILE_DIR/$BIN_NAME"

if [ ! -f "$TARGET_BIN" ]; then
  echo "未找到 $TARGET_BIN" >&2
  echo "请先执行: cargo build -p meter-ui --profile $PROFILE_ARG" >&2
  exit 1
fi

APP_BUNDLE="$ROOT_DIR/target/$PROFILE_DIR/$APP_NAME.app"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"

cp "$TARGET_BIN" "$APP_BUNDLE/Contents/MacOS/$BIN_NAME"
cp "$ROOT_DIR/meter-ui/assets/icon/app.icns" "$APP_BUNDLE/Contents/Resources/app.icns"
sed "s/__BIN_NAME__/$BIN_NAME/" "$SCRIPT_DIR/Info.plist.in" > "$APP_BUNDLE/Contents/Info.plist"

echo "已生成 $APP_BUNDLE"
echo "可以直接双击运行，或拖进 /Applications。"
