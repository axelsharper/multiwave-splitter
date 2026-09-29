#!/usr/bin/env bash
# Installs Multiwave Splitter for the current user (no root needed).
# Uninstall: ./install-linux.sh --uninstall
set -euo pipefail
cd "$(dirname "$0")"
BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
HICOLOR="$HOME/.local/share/icons/hicolor"
SIZES="16 24 32 48 64 128 256 512"

refresh() {
  # Make menus pick up the change right away.
  touch "$HICOLOR" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t -q "$HICOLOR" 2>/dev/null || true
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPS" 2>/dev/null || true
}

remove_icons() {
  for s in $SIZES 1024; do rm -f "$HICOLOR/${s}x${s}/apps/multiwave-splitter.png"; done
}

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN/multiwave-splitter" "$APPS/multiwave-splitter.desktop"
  remove_icons
  refresh
  echo "Multiwave Splitter removed."
  exit 0
fi

mkdir -p "$BIN" "$APPS"
remove_icons # also cleans up the misplaced 1024x1024 icon from version 0.1.0
install -m 755 multiwave-splitter "$BIN/multiwave-splitter"
for s in $SIZES; do
  install -D -m 644 "icons/${s}x${s}.png" "$HICOLOR/${s}x${s}/apps/multiwave-splitter.png"
done
# Absolute Exec path: ~/.local/bin is often not in PATH until the next login.
sed "s|^Exec=.*|Exec=\"$BIN/multiwave-splitter\" %f|" multiwave-splitter.desktop > "$APPS/multiwave-splitter.desktop"
chmod 644 "$APPS/multiwave-splitter.desktop"
refresh
echo "Installed. Start 'Multiwave Splitter' from your app menu, or run: $BIN/multiwave-splitter"
