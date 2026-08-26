#!/usr/bin/env sh
# Installs the icon theme files and desktop entry for the current user.
# Run with PREFIX=/usr/share (as root) for a system-wide install.
set -eu

PREFIX="${PREFIX:-$HOME/.local/share}"
SIZES="16 22 24 32 48 64 128 256 512"

# mkdir -p + cp rather than `install -D`: the -D flag is a GNU coreutils
# extension, and this script should also run under BSD install.
for size in $SIZES; do
  target="$PREFIX/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$target"
  cp "hicolor/${size}x${size}/apps/meterforge.png" "$target/meterforge.png"
  chmod 644 "$target/meterforge.png"
done

mkdir -p "$PREFIX/applications"
cp "meterforge.desktop" "$PREFIX/applications/meterforge.desktop"
chmod 644 "$PREFIX/applications/meterforge.desktop"

# Refresh the caches so the icon shows up without a re-login. Both tools are
# optional — a missing one is not an error.
gtk-update-icon-cache -f -t "$PREFIX/icons/hicolor" 2>/dev/null || true
update-desktop-database "$PREFIX/applications" 2>/dev/null || true

echo "Installed MeterForge icons into $PREFIX"
