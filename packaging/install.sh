#!/bin/sh
# Per-user install: binary, launcher entry, and icon. Run from the unpacked tarball.
set -e
PREFIX="${PREFIX:-$HOME/.local}"
install -Dm755 iron_renamer "$PREFIX/bin/iron_renamer"
install -Dm644 iron_renamer.desktop "$PREFIX/share/applications/iron_renamer.desktop"
install -Dm644 icon.png "$PREFIX/share/icons/hicolor/256x256/apps/iron_renamer.png"
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
gtk-update-icon-cache "$PREFIX/share/icons/hicolor" 2>/dev/null || true
echo "Installed to $PREFIX (make sure $PREFIX/bin is on your PATH)."
