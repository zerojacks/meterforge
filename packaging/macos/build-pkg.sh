#!/usr/bin/env sh
set -eu

APP_VERSION="${APP_VERSION:-0.1.0}"
DARWIN_ARCH="${DARWIN_ARCH:-x86_64}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
ROOT_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
APP_BUNDLE="$ROOT_DIR/target/release/MeterForge.app"
OUTPUT_DIR="$ROOT_DIR/dist"
OUTPUT_FILE="$OUTPUT_DIR/MeterForge-darwin-$DARWIN_ARCH-$APP_VERSION.pkg"

if [ ! -d "$APP_BUNDLE" ]; then
  echo "未找到 $APP_BUNDLE" >&2
  echo "请先执行 packaging/macos/build-app-bundle.sh" >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR"
pkgbuild \
  --component "$APP_BUNDLE" \
  --install-location /Applications \
  --version "$APP_VERSION" \
  "$OUTPUT_FILE"

echo "已生成 $OUTPUT_FILE"