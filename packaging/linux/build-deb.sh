#!/usr/bin/env sh
set -eu

VERSION="${1:-0.1.0}"
ARCH="${DEB_ARCH:-amd64}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
ROOT_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
PACKAGE_ROOT="$ROOT_DIR/target/deb/MeterForge"
OUTPUT_DIR="$ROOT_DIR/dist"
OUTPUT_FILE="$OUTPUT_DIR/MeterForge-linux-x86_64-$VERSION.deb"

if [ ! -f "$ROOT_DIR/target/release/MeterForge" ]; then
  echo "未找到 $ROOT_DIR/target/release/MeterForge" >&2
  echo "请先执行: cargo build --release -p meter-ui" >&2
  exit 1
fi

rm -rf "$PACKAGE_ROOT"
mkdir -p \
  "$PACKAGE_ROOT/DEBIAN" \
  "$PACKAGE_ROOT/usr/bin" \
  "$PACKAGE_ROOT/usr/share/applications"

for size in 16 22 24 32 48 64 128 256 512; do
  mkdir -p "$PACKAGE_ROOT/usr/share/icons/hicolor/${size}x${size}/apps"
  cp "$SCRIPT_DIR/hicolor/${size}x${size}/apps/meterforge.png" \
    "$PACKAGE_ROOT/usr/share/icons/hicolor/${size}x${size}/apps/meterforge.png"
done

cp "$ROOT_DIR/target/release/MeterForge" "$PACKAGE_ROOT/usr/bin/MeterForge"
chmod 755 "$PACKAGE_ROOT/usr/bin/MeterForge"
cp "$SCRIPT_DIR/meterforge.desktop" "$PACKAGE_ROOT/usr/share/applications/meterforge.desktop"
chmod 644 "$PACKAGE_ROOT/usr/share/applications/meterforge.desktop"

printf '%s\n' \
  'Package: meterforge' \
  "Version: $VERSION" \
  "Architecture: $ARCH" \
  'Maintainer: MeterForge Team' \
  'Section: utils' \
  'Priority: optional' \
  'Description: MeterForge virtual meter monitoring platform' \
  ' DL/T 645-2007 virtual electricity meter simulator and monitor.' \
  > "$PACKAGE_ROOT/DEBIAN/control"

mkdir -p "$OUTPUT_DIR"
dpkg-deb --build --root-owner-group "$PACKAGE_ROOT" "$OUTPUT_FILE"
echo "已生成 $OUTPUT_FILE"