#!/usr/bin/env bash
# Installs Multiwave Splitter for the current user (no root needed).
# Uninstall: ./install-linux.sh --uninstall
set -euo pipefail
cd "$(dirname "$0")"
BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/1024x1024/apps"
if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN/multiwave-splitter" "$APPS/multiwave-splitter.desktop" "$ICONS/multiwave-splitter.png"
  echo "Multiwave Splitter removed."
  exit 0
fi
mkdir -p "$BIN" "$APPS" "$ICONS"
install -m 755 multiwave-splitter "$BIN/multiwave-splitter"
install -m 644 multiwave-splitter.png "$ICONS/multiwave-splitter.png"
install -m 644 multiwave-splitter.desktop "$APPS/multiwave-splitter.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" || true
echo "Installed. Start 'Multiwave Splitter' from your app menu, or run: multiwave-splitter"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "Note: $BIN is not in your PATH." ;; esac
