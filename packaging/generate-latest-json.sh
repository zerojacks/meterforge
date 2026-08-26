#!/usr/bin/env sh
set -eu

VERSION="${1:?用法: sh packaging/generate-latest-json.sh <version> <repository> <artifacts-dir> [changelog] [output]}"
REPOSITORY="${2:?用法: sh packaging/generate-latest-json.sh <version> <repository> <artifacts-dir> [changelog] [output]}"
ARTIFACTS_DIR="${3:?用法: sh packaging/generate-latest-json.sh <version> <repository> <artifacts-dir> [changelog] [output]}"
CHANGELOG="${4:-CHANGELOG.md}"
OUTPUT="${5:-$ARTIFACTS_DIR/latest.json}"

asset_path() {
  printf '%s/%s' "$ARTIFACTS_DIR" "$1"
}

asset_sha256() {
  sha256sum "$1" | awk '{print $1}'
}

asset_size() {
  wc -c < "$1" | tr -d '[:space:]'
}

asset_json() {
  file="$1"
  url="$2"
  path="$(asset_path "$file")"
  if [ ! -f "$path" ]; then
    echo "未找到更新资产: $path" >&2
    exit 1
  fi
  jq -n \
    --arg url "https://github.com/$REPOSITORY/releases/latest/download/$file" \
    --arg sha256 "$(asset_sha256 "$path")" \
    --argjson size "$(asset_size "$path")" \
    '{url: $url, sha256: $sha256, size: $size}'
}

WINDOWS_FILE="MeterForge-Setup-$VERSION.exe"
LINUX_FILE="MeterForge-linux-x86_64-$VERSION.deb"
MAC_X86_FILE="MeterForge-darwin-x86_64-$VERSION.pkg"
MAC_X86_DMG="MeterForge-darwin-x86_64-$VERSION.dmg"
MAC_ARM_FILE="MeterForge-darwin-aarch64-$VERSION.pkg"
MAC_ARM_DMG="MeterForge-darwin-aarch64-$VERSION.dmg"

windows="$(asset_json "$WINDOWS_FILE" windows-x86_64)"
linux="$(asset_json "$LINUX_FILE" linux-x86_64)"
mac_x86="$(asset_json "$MAC_X86_FILE" darwin-x86_64)"
mac_x86_dmg="$(asset_json "$MAC_X86_DMG" darwin-x86_64-app)"
mac_arm="$(asset_json "$MAC_ARM_FILE" darwin-aarch64)"
mac_arm_dmg="$(asset_json "$MAC_ARM_DMG" darwin-aarch64-app)"

jq -n \
  --arg version "$VERSION" \
  --rawfile notes "$CHANGELOG" \
  --arg pub_date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson windows "$windows" \
  --argjson linux "$linux" \
  --argjson mac_x86 "$mac_x86" \
  --argjson mac_x86_dmg "$mac_x86_dmg" \
  --argjson mac_arm "$mac_arm" \
  --argjson mac_arm_dmg "$mac_arm_dmg" \
  '{
    version: $version,
    notes: $notes,
    pub_date: $pub_date,
    platforms: {
      "windows-x86_64": $windows,
      "windows-x86_64-nsis": $windows,
      "linux-x86_64": $linux,
      "darwin-x86_64": $mac_x86,
      "darwin-x86_64-app": $mac_x86_dmg,
      "darwin-aarch64": $mac_arm,
      "darwin-aarch64-app": $mac_arm_dmg
    }
  }' > "$OUTPUT"

echo "已生成 $OUTPUT"